//! Origin-bound pagination with aggregate memory and cycle limits.

use crate::{Client, Error, Method};
use serde::de::DeserializeOwned;
use std::collections::HashSet;

impl Client {
    /// Reads exactly one collection page. Next URLs remain subject to origin validation.
    ///
    /// # Errors
    /// Returns HTTP, transport, decoding, or unsafe-pagination errors.
    pub async fn page<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> Result<crate::models::CollectionPage<T>, Error> {
        let response = self.raw(Method::Get, path, query, None, &[]).await?;
        let page: crate::models::CollectionPage<T> = serde_json::from_slice(&response.body)?;
        if let Some(next) = &page.next {
            self.resolve_url(next)?;
        }
        Ok(page)
    }

    pub(crate) async fn list<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
        limit: usize,
    ) -> Result<Vec<T>, Error> {
        if limit == 0 {
            return Err(Error::invalid("limit must be positive"));
        }
        let mut next = self.resolve_url(path)?;
        next.query_pairs_mut()
            .append_pair("pagelen", &limit.min(100).to_string())
            .extend_pairs(query.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        let mut seen = HashSet::new();
        let mut values = Vec::new();
        let mut total_bytes = 0_usize;
        loop {
            if seen.len() >= 10_000 {
                return Err(Error::invalid(
                    "pagination exceeds 10000 pages; narrow the query",
                ));
            }
            if !seen.insert(next.to_string()) {
                return Err(Error::invalid("pagination loop detected"));
            }
            let response = self.raw(Method::Get, next.as_str(), &[], None, &[]).await?;
            total_bytes = total_bytes.saturating_add(response.body.len());
            if total_bytes > self.inner.max_body {
                return Err(Error::invalid(
                    "paginated JSON exceeds 64 MiB; narrow the query or lower the item limit",
                ));
            }
            let page: crate::models::Page<T> = serde_json::from_slice(&response.body)?;
            values.extend(page.values.into_iter().take(limit - values.len()));
            if values.len() >= limit {
                break;
            }
            match page.next {
                Some(path) => {
                    next = self.resolve_url(&path)?;
                }
                None => break,
            }
        }
        Ok(values)
    }
}
