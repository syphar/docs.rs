use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use docs_rs_storage::{FolderEntry, testing::open_archive_index};
use futures_util::{TryStreamExt as _, future::try_join_all};
use std::{fs::File, hint::black_box, path::Path, sync::Arc};
use tokio::runtime;

const FILE_COUNT: u64 = 80_827;
const HIT: &str = "Cargo.toml";
const DEEP_HIT: &str = "gh-pages/v0.0.4/libc/index.html";
const MISS: &str = "nonexistent/file.html";

fn archive_index_read(c: &mut Criterion) {
    let runtime = runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    // Share the existing large directory-listing fixture. Decompression and the
    // initial reads are setup costs, not part of any measured operation.
    let fixture_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shrust-0.0.4.index.zst");
    let mut index_file = tempfile::NamedTempFile::new().unwrap();
    zstd::stream::copy_decode(File::open(fixture_path).unwrap(), &mut index_file).unwrap();
    let index_path = Arc::new(index_file.into_temp_path());
    let mut index = runtime.block_on(open_archive_index(&index_path)).unwrap();

    runtime.block_on(async {
        assert_eq!(index.file_count().await.unwrap(), FILE_COUNT);
        assert!(index.find(HIT).await.unwrap().is_some());
        assert!(index.find(DEEP_HIT).await.unwrap().is_some());
        assert!(index.find(MISS).await.unwrap().is_none());
        assert_eq!(
            index.list().try_collect::<Vec<_>>().await.unwrap().len() as u64,
            FILE_COUNT
        );
    });

    // Isolate query costs on a connection that is already open, with warm file
    // system caches. Folder query costs are covered by the folder_contents bench.
    let mut group = c.benchmark_group("archive_index_read/reused_connection");
    group.throughput(Throughput::Elements(1));
    for (name, path) in [
        ("find_hit", HIT),
        ("find_deep_hit", DEEP_HIT),
        ("find_miss", MISS),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| black_box(runtime.block_on(index.find(black_box(path))).unwrap()));
        });
    }
    group.bench_function("file_count", |b| {
        b.iter(|| black_box(runtime.block_on(index.file_count()).unwrap()));
    });
    group.throughput(Throughput::Elements(FILE_COUNT));
    group.bench_function("list_all", |b| {
        b.iter(|| {
            black_box(
                runtime
                    .block_on(index.list().try_collect::<Vec<_>>())
                    .unwrap(),
            )
        });
    });
    group.finish();
    drop(index);

    // Measure standalone pools without the production pool cache. Include pool
    // initialization, acquisition, PRAGMAs, query, and dropping the connection.
    // The archive_index_cache bench covers production's shared pools.
    // No S3 downloads or cache repairs occur here.
    let mut group = c.benchmark_group("archive_index_read/fresh_connection");
    group.throughput(Throughput::Elements(1));
    group.bench_function("open", |b| {
        b.to_async(&runtime).iter(|| async {
            let index = open_archive_index(black_box(&index_path)).await.unwrap();
            drop(black_box(index));
        });
    });
    group.finish();

    let mut group = c.benchmark_group("archive_index_read/fresh_connection_concurrent");
    for readers in [1, 4, 16, 64] {
        // Each iteration waits for a complete batch of independent requests.
        // Throughput counts requests, while Criterion reports batch latency.
        group.throughput(Throughput::Elements(readers));
        for operation in ["find_hit", "find_miss", "folder_root"] {
            group.bench_function(BenchmarkId::new(operation, readers), |b| {
                b.to_async(&runtime).iter(|| async {
                    let tasks = (0..readers).map(|_| {
                        let path = Arc::clone(&index_path);
                        runtime.spawn(async move {
                            let mut index = open_archive_index(&path).await.unwrap();
                            match operation {
                                "find_hit" => {
                                    black_box(index.find(black_box(HIT)).await.unwrap());
                                }
                                "find_miss" => {
                                    black_box(index.find(black_box(MISS)).await.unwrap());
                                }
                                "folder_root" => {
                                    let entries: Vec<FolderEntry> = index
                                        .folder_contents(None::<&Path>)
                                        .try_collect()
                                        .await
                                        .unwrap();
                                    black_box(entries);
                                }
                                _ => unreachable!(),
                            }
                        })
                    });
                    try_join_all(tasks).await.unwrap();
                });
            });
        }
    }
    group.finish();
}

criterion_group!(archive_index_read_benches, archive_index_read);
criterion_main!(archive_index_read_benches);
