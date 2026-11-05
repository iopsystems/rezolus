//! A recording's identity as the dashboard reads it: the producer that made
//! it, the role it played (`service_kind`), and the name a user gave it
//! (`alias`), from its metadata alone, so both viewers, `recording check`
//! and MCP read it the same way.
//!
//! A recording with identity layers (metriken-storage `layers`, written by
//! metriken-recorder 0.6 and later) is read from its merged labels: the
//! producer is `__producer__`, and `service_kind`, `alias` and a `source`
//! label are what the user or a tool asserted. A recording without layers
//! is read from its `source` metadata key, as before; the legacy evidence
//! order of D2 (iopsystems/metriken
//! `docs/journal/2026-10-19-recording-identity-and-views.md`) is not
//! implemented here. In both, a `source` or `alias` that is a registry
//! template's name or alias gives that template's producer and
//! `service_kind` ([`Identity::kind`]).

use std::collections::{BTreeMap, HashMap};

use metriken_storage::layers::{Layers, PRODUCER_LABEL};

use crate::TemplateRegistry;
use crate::keys::KEY_SOURCE;
use crate::source_templates::is_one_source_name;

/// The label naming the role a producer played (`prefill`).
pub const SERVICE_KIND_LABEL: &str = "service_kind";
/// The label naming a recording as the user called it.
pub const ALIAS_LABEL: &str = "alias";
/// The producer name of the Rezolus agent.
pub const REZOLUS: &str = "rezolus";

/// What a recording's metadata says about who made it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// The producer the recording's layers name (`__producer__`).
    pub producer: Option<String>,
    /// The `service_kind` label.
    pub service_kind: Option<String>,
    /// The `alias` label.
    pub alias: Option<String>,
    /// A `source` the recording carries: a label in its layers, or, without
    /// layers, its `source` metadata key when that names one source.
    pub source: Option<String>,
    /// Whether the recording has identity layers.
    pub layered: bool,
}

impl Identity {
    /// The identity in a recording's metadata. Layers that do not parse are
    /// read as no layers.
    pub fn read(metadata: &HashMap<String, String>) -> Self {
        let sorted: BTreeMap<String, String> = metadata
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if let Ok(Some(layers)) = Layers::read(&sorted) {
            let labels = layers.merged().labels;
            let get = |key: &str| labels.get(key).filter(|v| !v.is_empty()).cloned();
            return Self {
                producer: get(PRODUCER_LABEL),
                service_kind: get(SERVICE_KIND_LABEL),
                alias: get(ALIAS_LABEL),
                source: get(KEY_SOURCE),
                layered: true,
            };
        }
        Self {
            source: metadata
                .get(KEY_SOURCE)
                .filter(|s| !s.is_empty() && is_one_source_name(s))
                .cloned(),
            ..Self::default()
        }
    }

    /// The identity in a recording's metadata given as a JSON object (the
    /// form the viewers pass to [`crate::source_kind::classify_sources`],
    /// with each value parsed as JSON where it is JSON); a value that is not
    /// a string is read as its JSON text, which is how a layer document was
    /// stored.
    pub fn read_json(metadata: &serde_json::Value) -> Self {
        let map: HashMap<String, String> = metadata
            .as_object()
            .into_iter()
            .flatten()
            .map(|(k, v)| {
                let text = v
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| v.to_string());
                (k.clone(), text)
            })
            .collect();
        Self::read(&map)
    }

    /// The producer and `service_kind` of the recording. The registry
    /// template its `source`, then its `alias`, names (by name or alias)
    /// gives a producer and a `service_kind` (a `vllm-prefill` source is
    /// producer `vllm`, kind `prefill`). The producer is the one the layers
    /// name, else that template's; the `service_kind` is the recording's
    /// label, else the template's when the template is for that producer.
    /// None when nothing names a producer.
    pub fn kind<'a>(
        &'a self,
        registry: &'a TemplateRegistry,
    ) -> Option<(&'a str, Option<&'a str>)> {
        let entry = [self.source.as_deref(), self.alias.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|name| registry.get(name));
        let producer = self.producer.as_deref().or(entry.map(|e| e.producer()))?;
        let entry_kind = entry
            .filter(|e| e.producer() == producer)
            .and_then(|e| e.service_kind.as_deref());
        Some((producer, self.service_kind.as_deref().or(entry_kind)))
    }

    /// Whether the recording names the Rezolus agent as its producer: its
    /// layers' `__producer__`, or, without layers, a `source` of `rezolus`.
    /// Other evidence a recording without layers can carry (the agent's
    /// `sampler_status`, its self-metric names) is
    /// [`crate::source_kind`]'s to weigh.
    pub fn is_rezolus(&self) -> bool {
        match (&self.producer, self.layered) {
            (Some(producer), _) => producer == REZOLUS,
            (None, true) => false,
            (None, false) => self.source.as_deref() == Some(REZOLUS),
        }
    }

    /// The name to show for the recording: its `alias`, else its `source`,
    /// else its producer.
    pub fn name(&self) -> Option<&str> {
        self.alias
            .as_deref()
            .or(self.source.as_deref())
            .or(self.producer.as_deref())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use metriken_storage::layers::{self, Entry, Layer, LegacyEvidence};

    /// A recording's metadata with identity layers: each `(layer, labels,
    /// metadata)` appended as one entry, as metriken-recorder writes them.
    pub(crate) fn layered(
        entries: &[(Layer, &[(&str, &str)], &[(&str, &str)])],
    ) -> HashMap<String, String> {
        let (mut labels, mut metadata) = (BTreeMap::new(), BTreeMap::new());
        for (n, (layer, l, m)) in entries.iter().enumerate() {
            let by = match layer {
                Layer::Recorder => "recorder",
                Layer::Producer => "producer",
                Layer::User => "record",
            };
            let mut entry = Entry::new(format!("e{n}"), by);
            entry.labels = l
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            entry.metadata = m
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            layers::append(
                &mut labels,
                &mut metadata,
                None,
                &LegacyEvidence::default(),
                *layer,
                entry,
            )
            .unwrap();
        }
        metadata.into_iter().collect()
    }

    fn legacy(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Without layers, the `source` key is the identity, a combined file's
    /// list of sources is not one, and a `source` that names a registry
    /// template gives that template's producer and kind.
    #[test]
    fn a_recording_without_layers_is_read_by_its_source() {
        let registry = crate::embedded_templates().unwrap();

        let prefill = Identity::read(&legacy(&[("source", "vllm-prefill")]));
        assert!(!prefill.layered);
        assert_eq!(prefill.source.as_deref(), Some("vllm-prefill"));
        assert_eq!(prefill.kind(&registry), Some(("vllm", Some("prefill"))));
        assert!(!prefill.is_rezolus());

        assert!(Identity::read(&legacy(&[("source", "rezolus")])).is_rezolus());
        let combined = Identity::read(&legacy(&[("source", r#"["rezolus","vllm"]"#)]));
        assert_eq!(combined.source, None);
        assert!(!combined.is_rezolus());
        assert_eq!(
            Identity::read(&legacy(&[("source", "web-01")])).kind(&registry),
            None
        );
    }

    /// With layers, the producer is the one the layers name, whatever
    /// `source` the user gave; a user `source` of `rezolus` without a
    /// producer does not make a recording the agent's; a recording that
    /// names no producer takes the kind its `source`, then its `alias`,
    /// names, refined by its own `service_kind`.
    #[test]
    fn a_recording_with_layers_is_read_by_its_producer() {
        let registry = crate::embedded_templates().unwrap();

        let agent = Identity::read(&layered(&[
            (Layer::Recorder, &[], &[("producer_name", "rezolus")]),
            (Layer::User, &[("source", "web-01")], &[]),
        ]));
        assert!(agent.layered);
        assert_eq!(agent.producer.as_deref(), Some("rezolus"));
        assert!(agent.is_rezolus());
        assert_eq!(agent.name(), Some("web-01"));

        let named_rezolus =
            Identity::read(&layered(&[(Layer::User, &[("source", "rezolus")], &[])]));
        assert!(named_rezolus.layered);
        assert!(!named_rezolus.is_rezolus());

        let decode = Identity::read(&layered(&[(Layer::User, &[("alias", "vllm-decode")], &[])]));
        assert_eq!(decode.kind(&registry), Some(("vllm", Some("decode"))));
        assert_eq!(decode.name(), Some("vllm-decode"));

        let refined = Identity::read(&layered(&[(
            Layer::User,
            &[("alias", "vllm"), ("service_kind", "prefill")],
            &[],
        )]));
        assert_eq!(refined.kind(&registry), Some(("vllm", Some("prefill"))));

        // A declared producer is refined by the template its `source` names
        // when that template is the producer's, and not by another's.
        let prefill_source = Identity::read(&layered(&[
            (Layer::Producer, &[], &[("producer_name", "vllm")]),
            (Layer::User, &[("source", "vllm-prefill")], &[]),
        ]));
        assert_eq!(
            prefill_source.kind(&registry),
            Some(("vllm", Some("prefill")))
        );
        let other = Identity::read(&layered(&[
            (Layer::Producer, &[], &[("producer_name", "cachecannon")]),
            (Layer::User, &[("alias", "vllm-prefill")], &[]),
        ]));
        assert_eq!(other.kind(&registry), Some(("cachecannon", None)));

        let declared = Identity::read(&layered(&[
            (Layer::Producer, &[], &[("producer_name", "cachecannon")]),
            (Layer::User, &[("service_kind", "client")], &[]),
        ]));
        assert_eq!(
            declared.kind(&registry),
            Some(("cachecannon", Some("client")))
        );
        assert_eq!(declared.name(), Some("cachecannon"));
    }
}
