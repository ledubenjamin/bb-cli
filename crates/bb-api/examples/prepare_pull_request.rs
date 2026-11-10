//! Builds and prints a validated request without contacting Bitbucket.

use std::io::{self, Write};

use bb_api::requests::{CreatePullRequest, Reviewer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let request =
        CreatePullRequest::new("Improve checks", "feature/checks".parse()?, "main".parse()?)?
            .description("Handle transport errors")
            .reviewers(vec![Reviewer::from_account_id("712020:example-account")?])
            .draft(true);

    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &request)?;
    writeln!(stdout)?;
    Ok(())
}
