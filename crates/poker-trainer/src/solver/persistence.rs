//! On-disk format for trained MCCFR blueprints.
//!
//! A blueprint file is a single MessagePack-encoded `BlueprintFile` record:
//!
//! ```text
//! { schema_version: u32, abstraction_tag: String, table: RegretTable }
//! ```
//!
//! Two compatibility gates run on load:
//!
//! 1. `schema_version` must equal `BLUEPRINT_SCHEMA_VERSION`. Bumped when the
//!    on-disk record shape itself changes (new fields, removed fields, type
//!    changes on existing fields).
//! 2. `abstraction_tag` must equal `ABSTRACTION_TAG`. Bumped whenever any
//!    decision that produces an `InfoSet` key changes — new abstract action,
//!    new card-bucket scheme, new raise size, anything that would silently
//!    re-interpret a stored key.
//!
//! A mismatch on either is a hard error. Silent reuse of a stale table would
//! produce a strategy that looks well-trained but is keyed off a different
//! game; better to fail loudly than to ship that.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::solver::regret::RegretTable;
#[cfg(test)]
use crate::solver::regret::RegretTableWire;

/// On-disk record schema version. Bump when `BlueprintFile`'s shape changes.
pub const BLUEPRINT_SCHEMA_VERSION: u32 = 1;

/// Tag identifying the abstraction the table was trained against. Bump when
/// any input to `InfoSet::from_observation` or the legal abstract action set
/// changes meaning. Format is intentionally a flat string so additions don't
/// require a parser — readers just compare for exact equality.
///
/// Current shape: 169-class preflop bucketing, single postflop bucket,
/// 4-action abstraction (Fold/Call/Raise/AllIn), single pot-sized raise.
pub const ABSTRACTION_TAG: &str = "preflop169.postflop1.actions4.raise=pot.v1";

#[derive(Serialize, Deserialize)]
struct BlueprintFile {
    schema_version: u32,
    abstraction_tag: String,
    table: RegretTable,
}

#[derive(Debug, Error)]
pub enum BlueprintLoadError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] rmp_serde::decode::Error),
    #[error(
        "blueprint schema version mismatch: file has {found}, this build expects {expected}"
    )]
    SchemaMismatch { expected: u32, found: u32 },
    #[error(
        "blueprint abstraction mismatch: file has \"{found}\", this build expects \"{expected}\""
    )]
    AbstractionMismatch { expected: String, found: String },
}

#[derive(Debug, Error)]
pub enum BlueprintSaveError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("encode error: {0}")]
    Encode(#[from] rmp_serde::encode::Error),
}

/// Save a trained `RegretTable` to `path`. Wraps it in a versioned envelope
/// (see module docs). Truncates and overwrites any existing file.
pub fn save_blueprint(table: &RegretTable, path: impl AsRef<Path>) -> Result<(), BlueprintSaveError> {
    let envelope = BlueprintFile {
        schema_version: BLUEPRINT_SCHEMA_VERSION,
        abstraction_tag: ABSTRACTION_TAG.to_string(),
        table: table.clone(),
    };
    let bytes = rmp_serde::to_vec(&envelope)?;
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

/// Load a blueprint previously written by `save_blueprint`. Errors if either
/// the schema version or the abstraction tag don't match the current build.
pub fn load_blueprint(path: impl AsRef<Path>) -> Result<RegretTable, BlueprintLoadError> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    let envelope: BlueprintFile = rmp_serde::from_slice(&bytes)?;
    if envelope.schema_version != BLUEPRINT_SCHEMA_VERSION {
        return Err(BlueprintLoadError::SchemaMismatch {
            expected: BLUEPRINT_SCHEMA_VERSION,
            found: envelope.schema_version,
        });
    }
    if envelope.abstraction_tag != ABSTRACTION_TAG {
        return Err(BlueprintLoadError::AbstractionMismatch {
            expected: ABSTRACTION_TAG.to_string(),
            found: envelope.abstraction_tag,
        });
    }
    Ok(envelope.table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use poker_engine::core::NaiveEvaluator;
    use poker_engine::game::{BettingRules, Engine};
    use crate::solver::mccfr::MccfrTrainer;
    use std::io::Write;

    fn tmp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        // Disambiguate per-process so parallel test runs don't collide.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        p.push(format!("blueprint_{}_{}_{}.bin", name, std::process::id(), nonce));
        p
    }

    fn trained_table() -> RegretTable {
        let engine = Engine::new(BettingRules::no_limit_holdem(1, 2, 0), NaiveEvaluator);
        let mut trainer = MccfrTrainer::new(13);
        let stacks = [200u32, 200];
        for i in 0..50u64 {
            let dealer = (i % 2) as poker_engine::game::SeatIndex;
            trainer.iterate(&engine, &stacks, dealer, i.wrapping_add(1));
        }
        trainer.table
    }

    #[test]
    fn roundtrip_preserves_entries() {
        let table = trained_table();
        let path = tmp_path("roundtrip");
        save_blueprint(&table, &path).expect("save");
        let loaded = load_blueprint(&path).expect("load");
        let _ = std::fs::remove_file(&path);

        assert_eq!(table.len(), loaded.len(), "entry count differs after round-trip");

        // Re-encode both via the wire form. The sorted multiset of
        // (key-bytes, value-bytes) must match — HashMap iteration order
        // is unspecified, but the contents must be identical.
        fn fingerprint(t: &RegretTable) -> Vec<Vec<u8>> {
            let bytes = rmp_serde::to_vec(t).unwrap();
            let wire: RegretTableWire = rmp_serde::from_slice(&bytes).unwrap();
            let mut frames: Vec<Vec<u8>> = wire
                .entries
                .iter()
                .map(|pair| rmp_serde::to_vec(pair).unwrap())
                .collect();
            frames.sort();
            frames
        }
        assert_eq!(fingerprint(&table), fingerprint(&loaded));
    }

    #[test]
    fn schema_mismatch_rejected() {
        let path = tmp_path("schema_mismatch");
        // Hand-craft a bad envelope.
        let bad = BlueprintFile {
            schema_version: BLUEPRINT_SCHEMA_VERSION + 99,
            abstraction_tag: ABSTRACTION_TAG.to_string(),
            table: RegretTable::new(),
        };
        let mut f = File::create(&path).unwrap();
        f.write_all(&rmp_serde::to_vec(&bad).unwrap()).unwrap();
        drop(f);

        let err = load_blueprint(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        match err {
            BlueprintLoadError::SchemaMismatch { .. } => {}
            other => panic!("expected SchemaMismatch, got {other:?}"),
        }
    }

    #[test]
    fn abstraction_mismatch_rejected() {
        let path = tmp_path("abstraction_mismatch");
        let bad = BlueprintFile {
            schema_version: BLUEPRINT_SCHEMA_VERSION,
            abstraction_tag: "preflop169.postflop1.actions4.raise=halfpot.v1".to_string(),
            table: RegretTable::new(),
        };
        let mut f = File::create(&path).unwrap();
        f.write_all(&rmp_serde::to_vec(&bad).unwrap()).unwrap();
        drop(f);

        let err = load_blueprint(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        match err {
            BlueprintLoadError::AbstractionMismatch { .. } => {}
            other => panic!("expected AbstractionMismatch, got {other:?}"),
        }
    }

    #[test]
    fn empty_table_roundtrips() {
        let table = RegretTable::new();
        let path = tmp_path("empty");
        save_blueprint(&table, &path).expect("save");
        let loaded = load_blueprint(&path).expect("load");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.len(), 0);
    }
}
