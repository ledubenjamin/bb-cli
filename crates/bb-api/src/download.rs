//! Bounded log requests with explicit byte offsets.

use crate::Error;
use std::ops::Range;

/// A byte range requested from a log endpoint.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ByteRange {
    /// Request the final number of bytes. The length must be positive.
    Suffix(u64),
    /// Request bytes starting at this absolute offset.
    From(u64),
    /// Request an inclusive interval.
    Inclusive {
        /// First requested byte.
        start: u64,
        /// Last requested byte, at or after start.
        end: u64,
    },
}

impl ByteRange {
    pub(crate) fn header(self) -> Result<String, Error> {
        match self {
            Self::Suffix(0) => Err(Error::invalid("log suffix length must be positive")),
            Self::Suffix(length) => Ok(format!("bytes=-{length}")),
            Self::From(offset) => Ok(format!("bytes={offset}-")),
            Self::Inclusive { start, end } if start <= end => Ok(format!("bytes={start}-{end}")),
            Self::Inclusive { .. } => Err(Error::invalid("invalid log byte interval")),
        }
    }
}

/// A streaming log response. Dropping it cancels the remaining download.
pub struct LogResponse {
    response: reqwest::Response,
    status: u16,
    range: Option<Range<u64>>,
    total: Option<u64>,
}

impl std::fmt::Debug for LogResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The response URL may carry a signed-storage credential.
        f.debug_struct("LogResponse")
            .field("status", &self.status)
            .field("range", &self.range)
            .field("total", &self.total)
            .finish_non_exhaustive()
    }
}

impl LogResponse {
    pub(crate) fn new(response: reqwest::Response) -> Result<Self, Error> {
        let status = response.status().as_u16();
        let (range, total) = if matches!(status, 206 | 416)
            && let Some(value) = response.headers().get("content-range")
        {
            parse_content_range(
                value
                    .to_str()
                    .map_err(|error| Error::invalid(format!("invalid Content-Range: {error}")))?,
            )?
        } else {
            (
                None,
                (status == 200).then(|| response.content_length()).flatten(),
            )
        };
        if status == 206 && range.is_none() {
            return Err(Error::invalid(
                "partial log response has no valid content range",
            ));
        }
        Ok(Self {
            response,
            status,
            range,
            total,
        })
    }

    /// Returns the HTTP status, including 304 and 416 when appropriate.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }
    /// Returns the received interval with an exclusive end, if the server honored Range.
    #[must_use]
    pub fn range(&self) -> Option<Range<u64>> {
        self.range.clone()
    }
    /// Returns the full remote log size, when known.
    #[must_use]
    pub fn total(&self) -> Option<u64> {
        self.total
    }
    /// Reads the next transport chunk without buffering the complete log.
    ///
    /// # Errors
    /// Returns a transport error when the download is interrupted.
    pub async fn chunk(&mut self) -> Result<Option<bytes::Bytes>, Error> {
        Ok(self.response.chunk().await?)
    }
}

fn parse_content_range(value: &str) -> Result<(Option<Range<u64>>, Option<u64>), Error> {
    let bad = || Error::invalid("invalid log Content-Range");
    let (interval, total) = value
        .strip_prefix("bytes ")
        .and_then(|s| s.split_once('/'))
        .ok_or_else(bad)?;
    let total = if total == "*" {
        None
    } else {
        Some(total.parse::<u64>()?)
    };
    if interval == "*" {
        return Ok((None, total));
    }
    let (start, end) = interval.split_once('-').ok_or_else(bad)?;
    let start = start.parse::<u64>()?;
    let end = end.parse::<u64>()?.checked_add(1).ok_or_else(bad)?;
    if end <= start || total.is_some_and(|total| end > total) {
        return Err(bad());
    }
    Ok((Some(start..end), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_partial_and_unsatisfied_intervals() -> Result<(), Error> {
        assert_eq!(
            parse_content_range("bytes 10-19/20")?,
            (Some(10..20), Some(20))
        );
        assert_eq!(parse_content_range("bytes */20")?, (None, Some(20)));
        parse_content_range("bytes 20-10/20")
            .err()
            .ok_or_else(|| Error::invalid("expected invalid interval"))?;
        parse_content_range("bytes 0-30/20")
            .err()
            .ok_or_else(|| Error::invalid("expected invalid total"))?;
        Ok(())
    }
}
