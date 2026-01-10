use anyhow::Result;

/// The main config trait for an application or library config.
///
/// Used across our various binary or library crates.
pub trait AppConfig: Sized {
    fn from_environment() -> Result<Self>;

    #[cfg(feature = "testing")]
    fn test_config() -> Result<Self> {
        Self::from_environment()
    }
}

pub trait AppConfigBuilder: Sized {
    type Config;
    type Loaded;

    fn load_environment(self) -> Result<Self::Loaded>;

    #[cfg(feature = "testing")]
    fn test_config(self) -> Result<Self::Loaded> {
        self.load_environment()
    }

    fn build(self) -> Self::Config;
}

// #[macro_export]
// macro_rules! impl_app_config_for_builder {
//     ($config:ty) => {
//         impl AppConfig for Config {
//             fn from_environment() -> Result<Self> {
//                 Ok(Self::builder().load_environment()?.build())
//             }

//             #[cfg(test)]
//             fn test_config() -> Result<Self> {
//                 Ok(Self::builder().test_config()?.build())
//             }
//         }
//     };
// }
