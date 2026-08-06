use super::*;

use tally_core::reader_state::{
    reader_state_path, set_reader_state, ReaderState, ReaderStateUpdate,
};

/// Every verb here writes straight to the reader-state file on disk — no
/// daemon socket, no RPC call. That is deliberate, not an oversight: the
/// property under test is that no *daemon* code path can touch this file,
/// and routing operator writes through the daemon at all would blur the line
/// this command family exists to keep bright.
pub(super) fn run_reader_state(command: ReaderStateCommand) -> Result<()> {
    match command {
        ReaderStateCommand::Archive {
            flow_run,
            tag,
            data_dir,
        } => {
            let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
            let record = set_reader_state(
                &reader_state_path(&data_dir),
                &flow_run,
                ReaderStateUpdate {
                    archived: Some(true),
                    triage_tag: tag.map(Some),
                },
            )
            .map_err(|error| invalid(error.to_string()))?;
            outln!("{}", serde_json::to_string(&record)?);
            Ok(())
        }
        ReaderStateCommand::Unarchive { flow_run, data_dir } => {
            let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
            let record = set_reader_state(
                &reader_state_path(&data_dir),
                &flow_run,
                ReaderStateUpdate {
                    archived: Some(false),
                    triage_tag: None,
                },
            )
            .map_err(|error| invalid(error.to_string()))?;
            outln!("{}", serde_json::to_string(&record)?);
            Ok(())
        }
        ReaderStateCommand::Tag {
            flow_run,
            tag,
            data_dir,
        } => {
            let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
            let record = set_reader_state(
                &reader_state_path(&data_dir),
                &flow_run,
                ReaderStateUpdate {
                    archived: None,
                    triage_tag: Some(Some(tag)),
                },
            )
            .map_err(|error| invalid(error.to_string()))?;
            outln!("{}", serde_json::to_string(&record)?);
            Ok(())
        }
        ReaderStateCommand::Untag { flow_run, data_dir } => {
            let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
            let record = set_reader_state(
                &reader_state_path(&data_dir),
                &flow_run,
                ReaderStateUpdate {
                    archived: None,
                    triage_tag: Some(None),
                },
            )
            .map_err(|error| invalid(error.to_string()))?;
            outln!("{}", serde_json::to_string(&record)?);
            Ok(())
        }
        ReaderStateCommand::Show { flow_run, data_dir } => {
            let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
            let state = ReaderState::read(&reader_state_path(&data_dir))
                .map_err(|error| invalid(error.to_string()))?;
            outln!("{}", serde_json::to_string(&state.record(&flow_run))?);
            Ok(())
        }
    }
}
