use crate::Error;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{fmt, num::NonZeroU64, str::FromStr};

macro_rules! number_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(transparent)]
        pub struct $name(NonZeroU64);
        impl $name {
            /// Returns the nonzero numeric identifier.
            pub fn get(self) -> u64 {
                self.0.get()
            }
        }
        impl FromStr for $name {
            type Err = Error;
            fn from_str(value: &str) -> Result<Self, Error> {
                Ok(Self(value.parse()?))
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}
number_id!(PullRequestId, "A repository-local pull request number.");
number_id!(CommentId, "A pull request comment identifier.");
number_id!(TaskId, "A pull request task identifier.");

macro_rules! text_id {
    ($name:ident, $description:literal, $validate:expr) => {
        #[doc = $description]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);
        impl $name {
            /// Borrows the validated text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl FromStr for $name {
            type Err = Error;
            fn from_str(value: &str) -> Result<Self, Error> {
                Self::try_from(value.to_owned())
            }
        }
        impl TryFrom<String> for $name {
            type Error = Error;
            fn try_from(value: String) -> Result<Self, Error> {
                if ($validate)(&value) {
                    Ok(Self(value))
                } else {
                    Err(Error::invalid(concat!("invalid ", stringify!($name))))
                }
            }
        }
        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

fn segment(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "/\\?#%".contains(c))
        && !value.starts_with('-')
}

text_id!(Workspace, "A Bitbucket workspace slug or UUID.", segment);
text_id!(ProjectKey, "A workspace-local project key.", segment);
text_id!(
    BranchName,
    "A Git branch name without a refs/heads prefix.",
    valid_branch
);

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Git ref names are case-sensitive and forbid exactly the .lock suffix"
)]
fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(['-', '/'])
        && !value.ends_with(['/', '.'])
        && !value.ends_with(".lock")
        && !value.contains("..")
        && !value.contains("@{")
        && !value.contains("//")
        && value != "@"
        && !value
            .split('/')
            .any(|part| part.starts_with('.') || part.ends_with(".lock"))
        && !value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
}

/// A validated workspace and repository pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct RepoRef {
    workspace: Workspace,
    slug: String,
}

impl RepoRef {
    /// Returns the workspace.
    #[must_use]
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    /// Returns the repository slug.
    #[must_use]
    pub fn slug(&self) -> &str {
        &self.slug
    }
    /// Returns the public Bitbucket repository URL.
    #[must_use]
    pub fn web_url(&self) -> String {
        format!("https://bitbucket.org/{self}")
    }
    /// Returns an API-relative repository resource path.
    #[must_use]
    pub fn path(&self) -> String {
        format!("repositories/{self}")
    }
}

impl FromStr for RepoRef {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self, Error> {
        let path = if let Some(path) = value.strip_prefix("git@bitbucket.org:") {
            path.to_owned()
        } else if value.contains("://") {
            let url = url::Url::parse(value)?;
            if !matches!(url.scheme(), "https" | "ssh")
                || !matches!(
                    url.host_str(),
                    Some("bitbucket.org" | "altssh.bitbucket.org")
                )
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || (url.scheme() == "https" && (!url.username().is_empty() || url.port().is_some()))
            {
                return Err(Error::invalid(
                    "expected a Bitbucket Cloud HTTPS or SSH repository URL",
                ));
            }
            url.path().trim_matches('/').to_owned()
        } else {
            value.to_owned()
        };
        let path = path.strip_suffix(".git").unwrap_or(&path);
        let (workspace, slug) = path
            .split_once('/')
            .ok_or_else(|| Error::invalid("repository must be workspace/repository"))?;
        if !segment(slug) {
            return Err(Error::invalid("invalid repository slug"));
        }
        Ok(Self {
            workspace: workspace.parse()?,
            slug: slug.to_owned(),
        })
    }
}
impl TryFrom<String> for RepoRef {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        value.parse()
    }
}
impl From<RepoRef> for String {
    fn from(value: RepoRef) -> Self {
        value.to_string()
    }
}
impl fmt::Display for RepoRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.workspace, self.slug)
    }
}
