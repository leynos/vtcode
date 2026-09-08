//! Aggregation of independent errors without short-circuiting work.

use std::fmt;

use anyhow::Error;

/// A collection of errors that enables continuing work while collecting failures.
///
/// This type implements the "error parameter" pattern: instead of short-circuiting
/// on the first error, processing continues and errors are accumulated. The caller
/// can inspect the collection afterwards to determine whether all operations
/// succeeded.
///
/// # Ergonomic `Result` handling
///
/// [`collect_result`](Self::collect_result) lets you process a `Result<T, E>`
/// while keeping the happy path dominant:
///
/// ```rust
/// use vtcode_commons::MultiErrors;
/// let mut errors: MultiErrors<String> = MultiErrors::new();
/// let value: Option<i32> = errors.collect_result("42".parse::<i32>().map_err(|error| error.to_string()));
/// assert_eq!(value, Some(42));
/// ```
///
/// # Composing with traditional error handling
///
/// Use [`ok`](Self::ok) or [`to_anyhow`](Self::to_anyhow) to convert back into
/// a traditional `Result`.
#[derive(Debug, Clone)]
pub struct MultiErrors<E = Error> {
    errors: Vec<E>,
}

impl<E> MultiErrors<E> {
    /// Create an empty error collection.
    pub fn new() -> Self {
        Self { errors: Vec::new() }
    }

    /// Add a single error to the collection.
    pub fn push(&mut self, error: E) {
        self.errors.push(error);
    }

    /// Extend the collection with multiple errors.
    pub(super) fn extend(&mut self, iter: impl IntoIterator<Item = E>) {
        self.errors.extend(iter);
    }

    /// Returns `true` if no errors have been collected.
    pub fn is_empty(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the number of collected errors.
    pub fn len(&self) -> usize {
        self.errors.len()
    }

    /// Consume the collector and return the underlying error vector.
    pub(super) fn into_inner(self) -> Vec<E> {
        self.errors
    }

    /// Returns a slice of all collected errors.
    pub fn as_slice(&self) -> &[E] {
        &self.errors
    }

    /// Returns an iterator over the collected errors.
    pub fn iter(&self) -> std::slice::Iter<'_, E> {
        self.errors.iter()
    }

    /// Convert into `Result<()>` — succeeds if no errors were collected.
    pub fn ok(self) -> Result<(), Self> {
        if self.errors.is_empty() { Ok(()) } else { Err(self) }
    }

    /// Remove all errors from the collection.
    pub fn clear(&mut self) {
        self.errors.clear();
    }

    /// Process a `Result`, returning the success value or collecting the error.
    ///
    /// This is the key ergonomic method — it keeps the happy path as the primary
    /// flow while silently collecting errors for later inspection.
    pub fn collect_result<T, F>(&mut self, result: Result<T, F>) -> Option<T>
    where
        F: Into<E>,
    {
        match result {
            Ok(val) => Some(val),
            Err(e) => {
                self.errors.push(e.into());
                None
            }
        }
    }

    /// Convert into an [`anyhow::Error`] for use with traditional error handling.
    pub fn to_anyhow(&self) -> Error
    where
        E: fmt::Display,
    {
        Error::msg(self.to_string())
    }
}

impl<E> Default for MultiErrors<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> From<Vec<E>> for MultiErrors<E> {
    fn from(errors: Vec<E>) -> Self {
        Self { errors }
    }
}

impl<E> IntoIterator for MultiErrors<E> {
    type Item = E;
    type IntoIter = std::vec::IntoIter<E>;

    fn into_iter(self) -> Self::IntoIter {
        self.errors.into_iter()
    }
}

impl<E: serde::Serialize> serde::Serialize for MultiErrors<E> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.errors.serialize(serializer)
    }
}

impl<'de, E: serde::Deserialize<'de>> serde::Deserialize<'de> for MultiErrors<E> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Vec::<E>::deserialize(deserializer).map(|errors| Self { errors })
    }
}

impl<E: fmt::Display> fmt::Display for MultiErrors<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.errors.len() {
            0 => write!(f, "no errors"),
            1 => write!(f, "{}", self.errors[0]),
            _ => {
                for (i, error) in self.errors.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                    }
                    write!(f, "  {}. {error}", i + 1)?;
                }
                Ok(())
            }
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for MultiErrors<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.errors.first().map(|e| e as &(dyn std::error::Error + 'static))
    }
}
