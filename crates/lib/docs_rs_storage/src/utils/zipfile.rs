use crate::utils::file_list::{FileItem, walk_dir_recursive};
use anyhow::{Result, anyhow};
use docs_rs_utils::spawn_blocking;
use futures_util::TryStreamExt as _;
use std::{
    fs,
    io::{self, Seek as _, Write as _},
    path::Path,
    thread,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use zip::write::SimpleFileOptions;

const BUFFER_SIZE: usize = 1024 * 1024;

fn compression_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Bzip2)
        .compression_level(Some(1))
}

fn build_subarchive(
    receiver: flume::Receiver<FileItem>,
    cancel: CancellationToken,
) -> Result<Option<fs::File>> {
    let tempfile = tempfile::tempfile()?;
    let mut archive = zip::ZipWriter::new(io::BufWriter::with_capacity(BUFFER_SIZE, tempfile));
    let mut file_count = 0_usize;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        let file = match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(file) => file,
            Err(flume::RecvTimeoutError::Timeout) => continue,
            Err(flume::RecvTimeoutError::Disconnected) => break,
        };

        if cancel.is_cancelled() {
            break;
        }

        archive.start_file(file.relative.to_string_lossy(), compression_options())?;

        let mut source = io::BufReader::with_capacity(BUFFER_SIZE, fs::File::open(&file.absolute)?);
        io::copy(&mut source, &mut archive)?;
        file_count += 1;
    }

    if file_count == 0 {
        return Ok(None);
    }

    let mut bufwriter = archive.finish()?;
    bufwriter.flush()?;
    let mut tempfile = bufwriter.into_inner()?;
    tempfile.rewind()?;
    Ok(Some(tempfile))
}

pub(crate) async fn archive_from_path(
    root: impl AsRef<Path>,
    cpu_parallelism: Option<usize>,
) -> Result<tempfile::NamedTempFile> {
    let root = root.as_ref();

    let worker_count = if let Some(p) = cpu_parallelism {
        p
    } else {
        thread::available_parallelism().map(|count| count.get())?
    };

    let (sender, receiver) = flume::bounded(worker_count.saturating_mul(2).max(1));
    let cancel = CancellationToken::new();
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let receiver = receiver.clone();
        let cancel = cancel.clone();
        let span = tracing::Span::current();
        workers.push(thread::spawn(move || {
            let _guard = span.enter();
            build_subarchive(receiver, cancel)
        }));
    }
    drop(receiver);

    let produce_result = walk_dir_recursive(&root)
        .try_for_each(|item| async {
            sender.send_async(item).await.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "zip workers stopped receiving")
            })
        })
        .await;

    if produce_result.is_err() {
        cancel.cancel();
    }
    drop(sender);

    let mut subarchives = Vec::new();
    let mut worker_error = None;
    for worker in workers {
        match worker.join() {
            Ok(result) => match result {
                Ok(Some(tempfile)) => subarchives.push(tempfile),
                Ok(None) => {}
                Err(err) if worker_error.is_none() => worker_error = Some(err),
                Err(_) => {}
            },
            Err(payload) if worker_error.is_none() => {
                worker_error = Some(anyhow!("panic joining thread: {:?}", payload));
            }
            Err(_) => {}
        }
    }

    if let Err(err) = produce_result {
        return Err(err.into());
    }

    if let Some(err) = worker_error {
        return Err(err);
    }

    let merged_archive_file = spawn_blocking(move || {
        let merged_archive_file = tempfile::NamedTempFile::new()?;
        let mut merged_archive = zip::ZipWriter::new(io::BufWriter::with_capacity(
            BUFFER_SIZE,
            merged_archive_file,
        ));
        for tempfile in subarchives {
            let subarchive =
                zip::ZipArchive::new(io::BufReader::with_capacity(BUFFER_SIZE, tempfile))?;
            merged_archive.merge_archive(subarchive)?;
        }

        let mut bufwriter = merged_archive.finish()?;
        bufwriter.flush()?;

        let mut tempfile = bufwriter.into_inner()?;
        tempfile.rewind()?;
        Ok(tempfile)
    })
    .await?;

    Ok(merged_archive_file)
}
