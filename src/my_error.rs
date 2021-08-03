use std::error;
use std::io::{Error, ErrorKind};
pub fn my_error<T>(err_str: T) -> Error
where
    T: Into<Box<dyn error::Error + Send + Sync>>,
{
    Error::new(ErrorKind::Other, err_str)
}
