//! This module is only used for internal purposes and does not contain any publicly usable
//! structs
#![allow(missing_docs)]

pub use iai_callgrind_runner::api::{
    Arg as InternalArg, Assistant as InternalAssistant, BinaryBenchmark as InternalBinaryBenchmark,
    BinaryBenchmarkConfig as InternalBinaryBenchmarkConfig,
    BinaryBenchmarkGroup as InternalBinaryBenchmarkGroup, Cmd as InternalCmd,
    Command as InternalCommand, ExitWith as InternalExitWith, Fixtures as InternalFixtures,
    FlamegraphConfig as InternalFlamegraphConfig, LibraryBenchmark as InternalLibraryBenchmark,
    LibraryBenchmarkBench as InternalLibraryBenchmarkBench,
    LibraryBenchmarkBenches as InternalLibraryBenchmarkBenches,
    LibraryBenchmarkConfig as InternalLibraryBenchmarkConfig,
    LibraryBenchmarkGroup as InternalLibraryBenchmarkGroup, RawArgs as InternalRawArgs,
    RegressionConfig as InternalRegressionConfig, Run as InternalRun, Tool as InternalTool,
    Tools as InternalTools,
};

#[derive(Debug, Clone)]
pub struct InternalMacroLibBench {
    pub id_display: Option<&'static str>,
    pub args_display: Option<&'static str>,
    pub func: fn(),
    pub config: Option<fn() -> crate::internal::InternalLibraryBenchmarkConfig>,
}

// TODO: Integrate setup, teardown functions ...
#[derive(Debug, Clone)]
pub struct InternalMacroBinBench {
    pub id_display: Option<&'static str>,
    pub args_display: Option<&'static str>,
    pub func: fn(),
    pub config: Option<fn() -> crate::internal::InternalLibraryBenchmarkConfig>,
}
