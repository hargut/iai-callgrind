# Customize the output directory

All output files of Iai-Callgrind are usually stored using the following scheme:

`$WORKSPACE_ROOT/target/iai/$PACKAGE_NAME/$BENCHMARK_FILE/$GROUP/$BENCH_FUNCTION.$BENCH_ID`

This directory structure can partly be changed with the following options.

## Callgrind Home

Per default, all benchmark output files are stored under the
`$WORKSPACE_ROOT/target/iai` directory tree. This home directory can be changed
with the `IAI_CALLGRIND_HOME` environment variable or the command-line argument
`--home`. The command-line argument overwrites the value of the environment
variable. For example to store all files under the `/tmp/iai-callgrind`
directory you can use `IAI_CALLGRIND_HOME=/tmp/iai-callgrind` or `cargo bench --
--home=/tmp/iai-callgrind`.

## Separate targets

If you're running the benchmarks on different targets, it's necessary to
separate the output files of the benchmark runs per target or else you could end
up comparing the benchmarks with the wrong target leading to strange results.
You can achieve this with different baselines per target, but it's much less
painful to separate the output files by target with the `--separate-targets`
command-line argument or setting the environment variable
`IAI_CALLGRIND_SEPARATE_TARGETS=yes`). The output directory structure
changes from

`target/iai/$PACKAGE_NAME/$BENCHMARK_FILE/$GROUP/$BENCH_FUNCTION.$BENCH_ID`

to

`target/iai/$TARGET_TRIPLE/$PACKAGE_NAME/$BENCHMARK_FILE/$GROUP/$BENCH_FUNCTION.$BENCH_ID`

For example, assuming the library benchmark file name is `bench_file` in the
package `my_package`

```rust
# extern crate iai_callgrind;
# mod my_lib { pub fn bubble_sort(_: Vec<i32>) -> Vec<i32> { vec![] } }
use iai_callgrind::{main, library_benchmark_group, library_benchmark};
use std::hint::black_box;

#[library_benchmark]
#[bench::short(vec![4, 3, 2, 1])]
fn bench_bubble_sort(values: Vec<i32>) -> Vec<i32> {
    black_box(my_lib::bubble_sort(values))
}

library_benchmark_group!(name = my_group; benchmarks = bench_bubble_sort);

# fn main() {
main!(library_benchmark_groups = my_group);
# }
```

Without `--separate-targets`:

`target/iai/my_package/bench_file/my_group/bench_bubble_sort.short`

and with `--separate-targets` assuming you're running the benchmark on the
`x86_64-unknown-linux-gnu` target:

`target/iai/x86_64-unknown-linux-gnu/my_package/bench_file/my_group/bench_bubble_sort.short`
