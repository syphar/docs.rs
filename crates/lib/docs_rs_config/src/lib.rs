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

pub trait EnvConfigBuilder: Sized {
    type Config;
    type Loaded: BuildConfig<Config = Self::Config>;

    fn load_environment(self) -> Result<Self::Loaded>;

    #[cfg(feature = "testing")]
    fn test_config(self) -> Result<Self::Loaded> {
        self.load_environment()
    }
}

pub trait BuildConfig: Sized {
    type Config;

    fn build(self) -> Result<Self::Config>;
}

pub trait HasBuilder: Sized {
    type Builder: EnvConfigBuilder<Config = Self>;

    fn builder() -> Result<Self::Builder>;
}

impl<T> AppConfig for T
where
    T: HasBuilder,
{
    fn from_environment() -> Result<Self> {
        T::builder()?.load_environment()?.build()
    }

    #[cfg(feature = "testing")]
    fn test_config() -> Result<Self> {
        T::builder()?.test_config()?.build()
    }
}
