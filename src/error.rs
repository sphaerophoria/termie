use std::{error::Error, fmt::Display};

pub struct FormattedError<'a>(&'a dyn Error);

impl Display for FormattedError<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let err = self.0;
        write!(f, "{}", err)?;

        if err.source().is_none() {
            return Ok(());
        }

        write!(f, "\nCaused by:")?;

        let mut source = err.source();
        while let Some(err) = source {
            write!(f, "\n{}", err)?;
            source = err.source();
        }

        Ok(())
    }
}

pub fn backtraced_err(err: &dyn Error) -> FormattedError<'_> {
    FormattedError(err)
}
