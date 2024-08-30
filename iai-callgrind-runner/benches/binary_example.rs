use std::thread;
use std::time::Duration;
use iai_callgrind::{main, BinaryBenchmarkConfig, binary_benchmark_group, binary_benchmark, Command, Stdin, Pipe};

#[binary_benchmark]
#[bench::thread_example()]
fn calling_child() -> Command {
    let path = env!("CARGO_BIN_EXE_thread_example");
    Command::new(path)
        .build()
}

binary_benchmark_group!(
    name = examples;
    config = BinaryBenchmarkConfig::default()
        .raw_callgrind_args([
            "--instr-atstart=no"
    ]);
    benchmarks = calling_child,
);

main!(
    binary_benchmark_groups = examples
);