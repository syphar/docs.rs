use crate::{
    InstanceMetrics,
    web::{
        page::{
            TemplateData,
            templates::{Body, Head, Vendored},
        },
        rustdoc::RustdocPage,
    },
};
use askama::Template;
use async_stream::stream;
use axum::body::Bytes;
use futures_util::{Stream, StreamExt as _};
use lol_html::{element, errors::RewritingError};
use std::sync::Arc;
use tokio::{io::AsyncRead, task::JoinHandle};
use tokio_util::io::ReaderStream;

/// Rewrite a rustdoc page to have the docs.rs topbar
///
/// Given a rustdoc HTML page and a context to serialize it with,
/// render the `rustdoc/` templates with the `html`.
/// The output is an HTML page which has not yet been UTF-8 validated.
/// In practice, the output should always be valid UTF-8.
pub(crate) async fn rewrite_rustdoc_html_stream<R>(
    template_data: Arc<TemplateData>,
    mut reader: R,
    max_allowed_memory_usage: usize,
    data: Arc<RustdocPage>,
    metrics: Arc<InstanceMetrics>,
) -> impl Stream<Item = Result<Bytes, RewritingError>>
where
    R: AsyncRead + Unpin + 'static,
{
    stream!({
        let (input_sender, input_receiver) = std::sync::mpsc::channel::<Option<Vec<u8>>>();
        let (result_sender, mut result_receiver) = tokio::sync::mpsc::unbounded_channel::<Bytes>();

        let join_handle: JoinHandle<anyhow::Result<_>> = tokio::spawn(async move {
            // we're using the rendering threadpool to limit CPU usage on the server, and to
            // offload potentially CPU intensive stuff from the tokio runtime.
            // Also this lets us limit the threadpool size and through that the CPU usage.
            template_data
                .render_in_threadpool(move || {
                    use lol_html::html_content::{ContentType, Element};
                    use lol_html::{HtmlRewriter, MemorySettings, Settings};

                    let head_html = Head::new(&data).render().unwrap();
                    let vendored_html = Vendored.render().unwrap();
                    let body_html = Body.render().unwrap();
                    let topbar_html = data.render().unwrap();

                    // Before: <body> ... rustdoc content ... </body>
                    // After:
                    // ```html
                    // <div id="rustdoc_body_wrapper" class="{{ rustdoc_body_class }}" tabindex="-1">
                    //      ... rustdoc content ...
                    // </div>
                    // ```
                    let body_handler = |rustdoc_body_class: &mut Element| {
                        // Add the `rustdoc` classes to the html body
                        let mut tmp;
                        let klass = if let Some(classes) = rustdoc_body_class.get_attribute("class")
                        {
                            tmp = classes;
                            tmp.push_str(" container-rustdoc");
                            &tmp
                        } else {
                            "container-rustdoc"
                        };
                        rustdoc_body_class.set_attribute("class", klass)?;
                        rustdoc_body_class.set_attribute("id", "rustdoc_body_wrapper")?;
                        rustdoc_body_class.set_attribute("tabindex", "-1")?;
                        // Change the `body` to a `div`
                        rustdoc_body_class.set_tag_name("div")?;
                        // Prepend the askama content
                        rustdoc_body_class.prepend(&body_html, ContentType::Html);
                        // Wrap the transformed body and topbar into a <body> element
                        rustdoc_body_class
                            .before(r#"<body class="rustdoc-page">"#, ContentType::Html);
                        // Insert the topbar outside of the rustdoc div
                        rustdoc_body_class.before(&topbar_html, ContentType::Html);
                        // Finalize body with </body>
                        rustdoc_body_class.after("</body>", ContentType::Html);

                        Ok(())
                    };

                    let settings = Settings {
                        element_content_handlers: vec![
                            // Append `style.css` stylesheet after all head elements.
                            element!("head", |head: &mut Element| {
                                head.append(&head_html, ContentType::Html);
                                Ok(())
                            }),
                            element!("body", body_handler),
                            // Append `vendored.css` before `rustdoc.css`, so that the duplicate copy of
                            // `normalize.css` will be overridden by the later version.
                            //
                            // Later rustdoc has `#mainThemeStyle` that could be used, but pre-2018 docs
                            // don't have this:
                            //
                            // https://github.com/rust-lang/rust/commit/003b2bc1c65251ec2fc80b78ed91c43fb35402ec
                            //
                            // Pre-2018 rustdoc also didn't have the resource suffix, but docs.rs was using a fork
                            // that had implemented it already then, so we can assume the css files are
                            // `<some path>/rustdoc-<some suffix>.css` and use the `-` to distinguish from the
                            // `rustdoc.static` path.
                            element!(
                                "link[rel='stylesheet'][href*='rustdoc-']",
                                move |rustdoc_css: &mut Element| {
                                    rustdoc_css.before(&vendored_html, ContentType::Html);
                                    Ok(())
                                }
                            ),
                        ],
                        memory_settings: MemorySettings {
                            max_allowed_memory_usage,
                            ..MemorySettings::default()
                        },
                        ..Settings::default()
                    };

                    let mut rewriter = HtmlRewriter::new(settings, move |chunk: &[u8]| {
                        // send the result back to the main rewriter when its coming in.
                        // this can fail only when the receiver is dropped, in which case
                        // we exit this thread anyways.
                        // FIXME: how to test this manually so we're sure?
                        let _ = result_sender.send(Bytes::from(chunk.to_vec()));
                    });
                    while let Some(chunk) = input_receiver.recv()? {
                        // receive data from the input receiver.
                        // `input_receiver` is a non-async one.
                        // Since we're in a normal background thread, we can use the blocking `.recv`
                        // here.
                        // We will get `None` when the reader is done reading,
                        // so that's our signal to exit this loop and call `rewriter.end()` below.
                        rewriter.write(&chunk)?;
                    }
                    // finalize everything. Will trigger the output sink (and through that,
                    // sending data to the `result_sender`).
                    rewriter.end()?;
                    Ok(())
                })
                .await?;
            Ok(())
        });

        let mut reader_stream = ReaderStream::new(&mut reader);
        while let Some(chunk) = reader_stream.next().await {
            let chunk = chunk.map_err(|err| {
                // FIXME: better error type
                RewritingError::ContentHandlerError(err.into())
            })?;

            if let Err(err) = input_sender.send(Some(chunk.to_vec())) {
                // FIXME: reveiver was dropped? do we care about that?
                yield Err(RewritingError::ContentHandlerError(err.into()));
                break;
            }

            while let Ok(bytes) = result_receiver.try_recv() {
                yield Ok(bytes);
            }
        }
        // This signals the renderer thread to finalize & exit.
        if let Err(err) = input_sender.send(None) {
            // FIXME: more explicit error type
            yield Err(RewritingError::ContentHandlerError(err.into()));
        }
        while let Some(bytes) = result_receiver.recv().await {
            yield Ok(bytes);
        }

        join_handle.await.expect("Task panicked").map_err(|e| {
            match e.downcast::<RewritingError>() {
                Ok(e) => {
                    if matches!(e, RewritingError::MemoryLimitExceeded(_)) {
                        metrics.html_rewrite_ooms.inc();
                    }
                    // FIXME: how to get axum to still generate that error with that message?
                    // return Err(AxumNope::InternalError(anyhow!(
                    //     "Failed to serve the rustdoc file '{}' because rewriting it surpassed the memory limit of {} bytes",
                    //     file_path,
                    //     config.max_parse_memory,
                    // )));
                    e
                }
                Err(e) => {
                    // FIXME: more explicit error type?
                    RewritingError::ContentHandlerError(e.into())
                }
            }
        })?;
    })
}

#[cfg(test)]
mod test {
    use crate::test::{AxumResponseTestExt, AxumRouterTestExt, async_wrapper};

    #[test]
    fn rewriting_only_injects_css_once() {
        async_wrapper(|env| async move {
            env.fake_release().await
                .name("testing")
                .version("0.1.0")
                // A somewhat representative rustdoc html file from 2016
                .rustdoc_file_with("2016/index.html", br#"
                    <html>
                        <head>
                            <meta charset="utf-8">
                            <link rel="stylesheet" type="text/css" href="../../../rustdoc-20160728-1.12.0-nightly-54c0dcfd6.css">
                            <link rel="stylesheet" type="text/css" href="../../../main-20160728-1.12.0-nightly-54c0dcfd6.css">
                        </head>
                    </html>
                "#)
                // A somewhat representative rustdoc html file from late 2022
                .rustdoc_file_with("2022/index.html", br#"
                    <html>
                        <head>
                            <meta charset="utf-8">
                            <link rel="preload" as="font" type="font/woff2" crossorigin="" href="/-/rustdoc.static/SourceSerif4-Regular-1f7d512b176f0f72.ttf.woff2">
                            <link rel="stylesheet" href="/-/rustdoc.static/normalize-76eba96aa4d2e634.css">
                            <link rel="stylesheet" href="/-/rustdoc.static/rustdoc-eabf764633b9d7be.css" id="mainThemeStyle">
                            <link rel="stylesheet" disabled="" href="/-/rustdoc.static/dark-e2f4109f2e82e3af.css">
                            <script src="/-/rustdoc.static/storage-d43fa987303ecbbb.js"></script>
                            <noscript><link rel="stylesheet" href="/-/rustdoc.static/noscript-13285aec31fa243e.css"></noscript>
                        </head>
                    </html>
                "#)
                .create().await?;

            let web = env.web_app().await;
            let output = web.get("/testing/0.1.0/2016/").await?.text().await?;
            assert_eq!(output.matches(r#"href="/-/static/vendored.css"#).count(), 1);

            let output = web.get("/testing/0.1.0/2022/").await?.text().await?;
            assert_eq!(output.matches(r#"href="/-/static/vendored.css"#).count(), 1);

            Ok(())
        });
    }
}
