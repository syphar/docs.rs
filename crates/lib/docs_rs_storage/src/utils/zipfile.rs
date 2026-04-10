use anyhow::{Result, bail};
use crossbeam_deque::{Injector, Steal};
use docs_rs_utils::spawn_blocking;
use futures_util::{StreamExt, TryStreamExt as _};
use std::{
    fs,
    io::{self, Seek as _, Write as _},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};
use tempfile::NamedTempFile;
use walkdir::WalkDir;
use zip::write::SimpleFileOptions;

use crate::utils::file_list::walk_dir_recursive;

const BUFFER_SIZE: usize = 1024 * 1024;

fn compression_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Bzip2)
        .compression_level(Some(1))
}

fn build_subarchive(
    injector: Arc<Injector<InputFile>>,
    producer_done: Arc<AtomicBool>,
) -> Result<Option<NamedTempFile>> {
    let tempfile = NamedTempFile::new()?;
    let mut archive = zip::ZipWriter::new(io::BufWriter::with_capacity(BUFFER_SIZE, tempfile));
    let mut file_count = 0_usize;

    loop {
        let file = loop {
            match injector.steal() {
                Steal::Success(file) => break Some(file),
                Steal::Retry => continue,
                Steal::Empty if producer_done.load(Ordering::Acquire) => break None,
                Steal::Empty => thread::yield_now(),
            }
        };

        let Some(file) = file else {
            break;
        };

        archive.start_file(&file.rel_path, compression_options())?;

        let mut source = io::BufReader::with_capacity(BUFFER_SIZE, fs::File::open(&file.abs_path)?);
        io::copy(&mut source, &mut archive)?;
        file_count += 1;
    }

    if file_count == 0 {
        return Ok(None);
    }

    let mut writer = archive.finish()?;
    writer.flush()?;
    let mut tempfile = writer.into_inner()?;
    tempfile.as_file_mut().rewind()?;
    Ok(Some(tempfile))
}

pub(crate) async fn archive_from_path(
    root: impl AsRef<Path>,
    cpu_parallelism: Option<usize>,
    filesystem_parallelism: usize,
) -> Result<tempfile::NamedTempFile> {
    let root = root.as_ref();

    let worker_count = if let Some(p) = cpu_parallelism {
        p
    } else {
        thread::available_parallelism().map(|count| count.get())?
    };

    let injector = Arc::new(Injector::new());
    let producer_done = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let injector = injector.clone();
        let producer_done = producer_done.clone();
        let span = tracing::Span::current();
        workers.push(thread::spawn(move || {
            let _guard = span.enter();
            build_subarchive(injector, producer_done)
        }));
    }

    walk_dir_recursive(&root)
        .try_for_each_concurrent(filesystem_parallelism, |item| async move {
            injector.push(item);
            Ok(())
        })
        .await?;

    producer_done.store(true, Ordering::Release);

    let mut subarchives = Vec::new();
    for worker in workers {
        match worker.join() {
            Ok(result) => {
                if let Some(tempfile) = result? {
                    subarchives.push(tempfile);
                }
            }
            Err(payload) => bail!("error joining thread: {:?}", payload),
        }
    }

    let merged_archive_file = spawn_blocking(move || {
        let merged_archive_file = tempfile::NamedTempFile::new()?;
        let mut merged_archive = zip::ZipWriter::new(std::io::BufWriter::with_capacity(
            BUFFER_SIZE,
            merged_archive_file,
        ));
        for tempfile in subarchives {
            let subarchive = zip::ZipArchive::new(tempfile.reopen()?)?;
            merged_archive.merge_archive(subarchive)?;
        }

        let writer = merged_archive.finish()?;
        writer.flush();

        Ok(writer.into_inner()?)
    })
    .await?;

    Ok(merged_archive_file)
}
