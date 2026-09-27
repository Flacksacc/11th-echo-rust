use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LocalModel {
    #[default]
    ParakeetTdtV2,
    ParakeetTdtV3,
    ParakeetUltra,
}

impl LocalModel {
    pub const ALL: [Self; 3] = [
        Self::ParakeetTdtV2,
        Self::ParakeetTdtV3,
        Self::ParakeetUltra,
    ];

    pub fn from_id(value: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|model| value.trim() == model.id() || value.trim() == model.label())
            .unwrap_or_default()
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::ParakeetTdtV2 => "parakeet-tdt-0.6b-v2-int8",
            Self::ParakeetTdtV3 => "parakeet-tdt-0.6b-v3-int8",
            Self::ParakeetUltra => "parakeet-ultra",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::ParakeetTdtV2 => "Parakeet TDT 0.6B v2 — 600M parameters, English, INT8",
            Self::ParakeetTdtV3 => "Parakeet TDT 0.6B v3 — 600M parameters, 25 languages, INT8",
            Self::ParakeetUltra => "Parakeet Ultra — NVIDIA GPU, 25 languages",
        }
    }

    pub fn uses_gpu(self) -> bool {
        self == Self::ParakeetUltra
    }

    pub(super) fn archive_url(self) -> &'static str {
        match self {
            Self::ParakeetTdtV2 => "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8.tar.bz2",
            Self::ParakeetTdtV3 => "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2",
            Self::ParakeetUltra => "",
        }
    }

    pub(super) fn archive_sha256(self) -> &'static str {
        match self {
            Self::ParakeetTdtV2 => {
                "157c157bc51155e03e37d2466522a3a737dd9c72bb25f36eb18912964161e1ad"
            }
            Self::ParakeetTdtV3 => {
                "5793d0fd397c5778d2cf2126994d58e9d56b1be7c04d13c7a15bb1b4eafb16bf"
            }
            Self::ParakeetUltra => "",
        }
    }

    pub(super) fn archive_size(self) -> u64 {
        match self {
            Self::ParakeetTdtV2 => 482_468_385,
            Self::ParakeetTdtV3 => 487_170_055,
            Self::ParakeetUltra => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn models_round_trip_and_keep_legacy_default() {
        for model in LocalModel::ALL {
            assert_eq!(LocalModel::from_id(model.id()), model);
            assert_eq!(LocalModel::from_id(model.label()), model);
            assert_eq!(
                serde_json::from_str::<LocalModel>(&serde_json::to_string(&model).unwrap())
                    .unwrap(),
                model
            );
        }
        assert_eq!(LocalModel::from_id("unknown"), LocalModel::ParakeetTdtV2);
    }
}
