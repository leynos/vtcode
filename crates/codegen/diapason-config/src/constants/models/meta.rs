//! Official Meta AI Muse models exposed by Diapason.

/// Meta Muse Spark 1.1.
pub const MUSE_SPARK_1_1: &str = "muse-spark-1.1";
/// Meta Muse Spark 1.2.
pub const MUSE_SPARK_1_2: &str = "muse-spark-1.2";
/// Meta Muse Spark 1.2 Contributor-tier variant.
pub const MUSE_SPARK_1_2_CONTRIBUTOR: &str = "muse-spark-1.2-contributor";
/// Meta Muse Spark 1.3.
pub const MUSE_SPARK_1_3: &str = "muse-spark-1.3";
/// Meta Muse Spark 1.3 Contributor-tier variant.
pub const MUSE_SPARK_1_3_CONTRIBUTOR: &str = "muse-spark-1.3-contributor";

/// Default Meta AI model.
pub const DEFAULT_MODEL: &str = MUSE_SPARK_1_3;

/// Official Meta AI models supported by Diapason.
pub const SUPPORTED_MODELS: &[&str] = &[
    MUSE_SPARK_1_1,
    MUSE_SPARK_1_2,
    MUSE_SPARK_1_2_CONTRIBUTOR,
    MUSE_SPARK_1_3,
    MUSE_SPARK_1_3_CONTRIBUTOR,
];

/// Meta Muse models use always-on reasoning.
pub const REASONING_MODELS: &[&str] = SUPPORTED_MODELS;
