//! Trusting a repository's own slot types (ADR 0028). A repo may ship
//! `.berth/slots/<type>/`, and its `slot.sh` runs on this Mac with the
//! connector keys in its environment, so Berth runs one only once the user
//! trusted it, as its files are: a change to any of them asks again.

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use berth_slot::{SlotType, SlotTypeSource};

use super::{Shared, SlotTrust, SlotTypeInfo, UiEvent, lock};

/// Setting: `{ "<repo id>/<type>": "<fingerprint>" }`.
const KEY: &str = "trusted_slot_types";

impl Shared {
    /// Whether Berth runs `ty`'s script for the repository `repo_id`.
    pub(super) fn slot_trust(&self, repo_id: &str, ty: &SlotType) -> SlotTrust {
        if ty.source != SlotTypeSource::Repo {
            return SlotTrust::Own;
        }
        let Some(trusted) = self.trusted_slot_types().remove(&key(repo_id, &ty.name)) else {
            return SlotTrust::Untrusted;
        };
        match ty.fingerprint() {
            Ok(print) if print == trusted => SlotTrust::Trusted,
            Ok(_) => SlotTrust::Changed,
            Err(e) => {
                tracing::warn!(error = %e, slot_type = %ty.name, "slot type fingerprint");
                SlotTrust::Changed
            }
        }
    }

    /// Nothing runs a script the user hasn't trusted: an error that says
    /// why, unless `ty` may run.
    pub(super) fn require_trusted(&self, repo_id: &str, ty: &SlotType) -> Result<()> {
        let dir = ty.dir.display();
        match self.slot_trust(repo_id, ty) {
            SlotTrust::Own | SlotTrust::Trusted => Ok(()),
            SlotTrust::Untrusted => Err(anyhow!(
                "`{}` is this repository's own slot type ({dir}): its script runs on this Mac \
                 with your connector keys. Trust it in New workflow before using it.",
                ty.name
            )),
            SlotTrust::Changed => Err(anyhow!(
                "This repository's slot type `{}` changed since you trusted it ({dir}). Trust it \
                 again in New workflow to run it.",
                ty.name
            )),
        }
    }

    /// The user trusts the repository's type `name` as its files are now.
    pub(super) fn trust_slot_type(&self, repo_id: &str, name: &str) -> Result<()> {
        let path = self.repo_path(repo_id)?;
        let found = berth_slot::discover_slot_types(&berth_slot::global_slots_dir(), &path);
        let ty = found
            .types
            .iter()
            .find(|t| t.name == name && t.source == SlotTypeSource::Repo)
            .ok_or_else(|| anyhow!("this repository has no slot type `{name}`"))?;
        let mut all = self.trusted_slot_types();
        all.insert(key(repo_id, name), ty.fingerprint()?);
        self.core()
            .store
            .set_setting(KEY, &serde_json::to_string(&all)?)?;
        tracing::info!(
            repo_id,
            slot_type = name,
            "trusted the repository's slot type"
        );
        let slot_types = self.slot_type_infos(repo_id, &found.types);
        if let Some(ctx) = lock(&self.repos).get_mut(repo_id) {
            ctx.slot_types = found.types;
        }
        self.emit(UiEvent::SlotTypes {
            repo_id: repo_id.to_owned(),
            slot_types,
        });
        Ok(())
    }

    /// The repository's slot types as New workflow shows them.
    pub(super) fn slot_type_infos(&self, repo_id: &str, types: &[SlotType]) -> Vec<SlotTypeInfo> {
        types
            .iter()
            .map(|t| SlotTypeInfo {
                name: t.name.clone(),
                description: t.description.clone(),
                placement: t.agent_placement.as_str().to_owned(),
                from_repo: t.source == SlotTypeSource::Repo,
                dir: t.dir.clone(),
                trust: self.slot_trust(repo_id, t),
            })
            .collect()
    }

    fn trusted_slot_types(&self) -> BTreeMap<String, String> {
        let value = self.core().store.setting(KEY).ok().flatten();
        value
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default()
    }
}

fn key(repo_id: &str, name: &str) -> String {
    format!("{repo_id}/{name}")
}
