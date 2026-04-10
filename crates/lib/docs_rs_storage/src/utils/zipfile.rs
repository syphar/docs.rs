use crate::utils::file_list::{FileItem, walk_dir_recursive};
use anyhow::Result;
use docs_rs_utils::spawn_blocking;
use futures_util::TryStreamExt as _;
use std::{
    collections::VecDeque,
    fs, future,
    io::{self, Seek as _, Write as _},
    path::Path,
    sync::{Arc, Condvar, Mutex},
    thread,
};
use zip::write::SimpleFileOptions;

const BUFFER_SIZE: usize = 1024 * 1024;

fn compression_options() -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Bzip2)
        .compression_level(Some(1))
}

struct QueueState {
    files: VecDeque<FileItem>,
    producer_done: bool,
}

struct WorkQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl WorkQueue {
    fn new() -> Self {
        Self {
            state: Mutex::new(QueueState {
                files: VecDeque::new(),
                producer_done: false,
            }),
            ready: Condvar::new(),
        }
    }

    fn push(&self, file: FileItem) {
        let mut state = self.state.lock().unwrap();
        state.files.push_back(file);
        self.ready.notify_one();
    }

    fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.producer_done = true;
        self.ready.notify_all();
    }

    fn pop(&self) -> Option<FileItem> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(file) = state.files.pop_front() {
                return Some(file);
            }

            if state.producer_done {
                return None;
            }

            state = self.ready.wait(state).unwrap();
        }
    }
}

fn build_subarchive(queue: Arc<WorkQueue>) -> Result<Option<fs::File>> {
    let tempfile = tempfile::tempfile()?;
    let mut archive = zip::ZipWriter::new(io::BufWriter::with_capacity(BUFFER_SIZE, tempfile));
    let mut file_count = 0_usize;

    while let Some(file) = queue.pop() {
        archive.start_file(file.relative.to_string_lossy(), compression_options())?;

        let mut source = io::BufReader::with_capacity(BUFFER_SIZE, fs::File::open(&file.absolute)?);
        io::copy(&mut source, &mut archive)?;
        file_count += 1;
    }

    if file_count == 0 {
        return Ok(None);
    }

    let mut writer = archive.finish()?;
    writer.flush()?;
    let mut tempfile = writer.into_inner()?;
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

    let queue = Arc::new(WorkQueue::new());
    let mut workers = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let queue = queue.clone();
        let span = tracing::Span::current();
        workers.push(thread::spawn(move || {
            let _guard = span.enter();
            build_subarchive(queue)
        }));
    }

    let produce_result = walk_dir_recursive(&root)
        .try_for_each(|item| {
            queue.push(item);
            future::ready(Ok(()))
        })
        .await;

    queue.close();

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
                worker_error = Some(anyhow::anyhow!("error joining thread: {:?}", payload));
            }
            Err(_) => {}
        }
    }

    produce_result?;

    if let Some(err) = worker_error {
        return Err(err);
    }

    let merged_archive_file = spawn_blocking(move || {
        let merged_archive_file = tempfile::NamedTempFile::new()?;
        let mut merged_archive = zip::ZipWriter::new(std::io::BufWriter::with_capacity(
            BUFFER_SIZE,
            merged_archive_file,
        ));
        for tempfile in subarchives {
            let subarchive =
                zip::ZipArchive::new(io::BufReader::with_capacity(BUFFER_SIZE, tempfile))?;
            merged_archive.merge_archive(subarchive)?;
        }

        let mut writer = merged_archive.finish()?;
        writer.flush()?;

        Ok(writer.into_inner()?)
    })
    .await?;

    Ok(merged_archive_file)
}
