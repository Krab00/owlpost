//! `owl reject <id>`: discard an inbox record — moved to `done/` with state `rejected` (§8).

use std::path::Path;

use anyhow::Context;
use owlpost::envelope;
use owlpost::spool::{Dir, Spool};
use serde_json::json;

use super::{inbox_record, print_json};

pub fn run(home: &Path, id: &str, json: bool) -> anyhow::Result<()> {
    let spool = Spool::new(home)?;
    let rec = inbox_record(&spool, id)?;
    spool.move_to(Dir::Inbox, id, Dir::Done)?;
    let mut done = spool
        .get(Dir::Done, id)?
        .with_context(|| format!("record {id} vanished from done/"))?;
    done.state = "rejected".into();
    done.meta["previous_state"] = json!(rec.state);
    done.meta["done_at"] = json!(envelope::rfc3339_now());
    spool.put(Dir::Done, id, &done)?;
    if json {
        print_json(&json!({ "id": id, "state": "rejected" }))?;
    } else {
        println!("rejected {id}");
    }
    Ok(())
}
