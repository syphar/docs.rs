pub(crate) mod axum_helpers;
pub(crate) mod headers;
mod test_env;

pub(crate) use axum_helpers::{AxumResponseTestExt, AxumRouterTestExt, assert_cache_headers_eq};
pub(crate) use test_env::TestEnvironment;

// pub(crate) fn async_wrapper<F, Fut>(f: F)
// where
//     F: FnOnce(Rc<TestEnvironment>) -> Fut,
//     Fut: Future<Output = Result<()>>,
// {
//     todo!();
//     // let env = Rc::new(
//     //     TestEnvironment::with_config_and_runtime(TestEnvironment::base_config().build().unwrap())
//     //         .unwrap(),
//     // );

//     // env.runtime().block_on(f(env.clone())).expect("test failed");
// }

// pub(crate) struct TestEnvironment {
//     // NOTE: the database & storage have to come before the context,
//     // otherwise it can happen that we can't cleanup the test database
//     // because the tokio runtime from the context is gone.
//     db: TestDatabase,
//     _storage: TestStorage,
//     pub context: Context,
//     owned_runtime: Option<Arc<runtime::Runtime>>,
//     test_metrics: TestMetrics,
// }
//
// impl TestEnvironment {
//     pub(crate) async fn new() -> Result<Self> {
//         Self::with_config(Self::base_config().build()?).await
//     }

//     pub(crate) fn with_config_and_runtime(config: Config) -> Result<Self> {
//         let runtime = Arc::new(
//             runtime::Builder::new_multi_thread()
//                 .enable_all()
//                 .build()
//                 .context("failed to initialize runtime")?,
//         );
//         let mut env = runtime.block_on(Self::with_config(config))?;
//         env.owned_runtime = Some(runtime);
//         Ok(env)
//     }

//     pub(crate) async fn with_config(config: Config) -> Result<Self> {
//         init_logger();

//         // create index directory
//         fs::create_dir_all(config.watcher.registry_index_path.clone())?;

//         let test_metrics = TestMetrics::new();
//         let test_db = TestDatabase::new(&config.database, test_metrics.provider())
//             .await
//             .context("can't initialize test database")?;

//         let test_storage =
//             TestStorage::from_config(config.storage.clone(), test_metrics.provider())
//                 .await
//                 .context("can't initialize test storage")?;

//         Ok(Self {
//             context: Context::from_test_config(
//                 config,
//                 test_metrics.provider().clone(),
//                 test_db.pool().clone(),
//                 test_storage.storage(),
//             )
//             .await?,
//             db: test_db,
//             _storage: test_storage,
//             owned_runtime: None,
//             test_metrics,
//         })
//     }

//     pub(crate) fn base_config() -> ConfigBuilder {
//         Config::from_env()
//             .expect("can't load base config from environment")
//             .database(
//                 docs_rs_database::Config::test_config()
//                     .expect("can't load database config")
//                     .into(),
//             )
//             .storage(
//                 docs_rs_storage::Config::test_config(StorageKind::Memory)
//                     .expect("can't load storage config")
//                     .into(),
//             )
//             .builder(
//                 docs_rs_builder::Config::test_config()
//                     .expect("can't load builder config")
//                     .into(),
//             )
//             // set stale content serving so Cache::ForeverInCdn and Cache::ForeverInCdnAndStaleInBrowser
//             // are actually different.
//             .cache_control_stale_while_revalidate(Some(86400))
//     }

//     pub(crate) fn async_build_queue(&self) -> &AsyncBuildQueue {
//         &self.context.async_build_queue
//     }

//     pub(crate) fn config(&self) -> &Config {
//         &self.context.config
//     }

//     pub(crate) fn async_storage(&self) -> &AsyncStorage {
//         &self.context.async_storage
//     }

//     pub(crate) fn runtime(&self) -> &runtime::Handle {
//         &self.context.runtime
//     }

//     pub(crate) fn async_db(&self) -> &TestDatabase {
//         &self.db
//     }

//     pub(crate) fn collected_metrics(&self) -> CollectedMetrics {
//         self.test_metrics.collected_metrics()
//     }

//     pub(crate) async fn web_app(&self) -> Router {
//         let template_data = Arc::new(TemplateData::new(1).unwrap());
//         build_axum_app(&self.context, template_data)
//             .await
//             .expect("could not build axum app")
//     }

//     pub(crate) async fn fake_release(&self) -> FakeRelease<'_> {
//         FakeRelease::new(self.db.pool().clone(), self.context.async_storage.clone())
//     }
// }
