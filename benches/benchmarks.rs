//! Benchmarks for MiniLang compiler components
//!
//! Run with: cargo bench
//! Profile with: cargo bench -- --profile-time=5
//!
//! For perf profiling:
//!   cargo build --release
//!   perf record -g ./target/release/minilang examples/fibonacci.lang --bench
//!   perf report

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use minilang::{
    BumpAllocator, Compiler, FreeListAllocator, GarbageCollector, Lexer, Parser, SemanticAnalyzer,
    SlabAllocator, TypeTag, Vm,
};

const SIMPLE_PROGRAM: &str = "func main() { return 42; }";

const FIBONACCI: &str = r#"
func fib(int n) {
    if (n <= 1) { return n; }
    return fib(n - 1) + fib(n - 2);
}
func main() { return fib(20); }
"#;

const LOOP_PROGRAM: &str = r#"
func main() {
    int i = 0;
    int sum = 0;
    while (i < 1000) {
        sum = sum + i;
        i = i + 1;
    }
    return sum;
}
"#;

fn bench_lexer(c: &mut Criterion) {
    let mut group = c.benchmark_group("lexer");

    group.bench_function("simple", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(black_box(SIMPLE_PROGRAM));
            lexer.tokenize()
        })
    });

    group.bench_function("fibonacci", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(black_box(FIBONACCI));
            lexer.tokenize()
        })
    });

    group.finish();
}

fn bench_parser(c: &mut Criterion) {
    let mut group = c.benchmark_group("parser");

    // Pre-tokenize
    let mut lexer = Lexer::new(FIBONACCI);
    let tokens = lexer.tokenize();

    group.bench_function("fibonacci", |b| {
        b.iter(|| {
            let mut parser = Parser::new(black_box(tokens.clone()));
            parser.parse()
        })
    });

    group.finish();
}

fn bench_full_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("full_pipeline");

    group.bench_function("simple", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(SIMPLE_PROGRAM);
            let tokens = lexer.tokenize();
            let mut parser = Parser::new(tokens);
            let program = parser.parse().unwrap();
            let mut analyzer = SemanticAnalyzer::new();
            analyzer.analyze(&program).unwrap();
            let (compiled, _) = Compiler::new().compile(&program);
            let mut vm = Vm::new(&compiled);
            vm.run()
        })
    });

    group.bench_function("loop_1000", |b| {
        b.iter(|| {
            let mut lexer = Lexer::new(LOOP_PROGRAM);
            let tokens = lexer.tokenize();
            let mut parser = Parser::new(tokens);
            let program = parser.parse().unwrap();
            let mut analyzer = SemanticAnalyzer::new();
            analyzer.analyze(&program).unwrap();
            let (compiled, _) = Compiler::new().compile(&program);
            let mut vm = Vm::new(&compiled).with_max_cycles(1_000_000);
            vm.run()
        })
    });

    group.finish();
}

fn bench_vm_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("vm_execution");

    // Pre-compile
    let mut lexer = Lexer::new(LOOP_PROGRAM);
    let tokens = lexer.tokenize();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let mut analyzer = SemanticAnalyzer::new();
    analyzer.analyze(&program).unwrap();
    let (compiled, _) = Compiler::new().compile(&program);

    group.bench_function("loop_1000", |b| {
        b.iter(|| {
            let mut vm = Vm::new(black_box(&compiled)).with_max_cycles(1_000_000);
            vm.run()
        })
    });

    group.finish();
}

fn bench_allocators(c: &mut Criterion) {
    let mut group = c.benchmark_group("allocators");

    // Bump allocator
    group.bench_function("bump_alloc_1000", |b| {
        let bump = BumpAllocator::new(1024 * 1024);
        b.iter(|| {
            for _ in 0..1000 {
                black_box(bump.alloc(64));
            }
            bump.reset();
        })
    });

    // Free-list allocator
    group.bench_function("freelist_alloc_free_100", |b| {
        let fl = FreeListAllocator::new(1024 * 1024);
        b.iter(|| {
            let mut ptrs = Vec::with_capacity(100);
            for _ in 0..100 {
                ptrs.push(fl.alloc(64).unwrap());
            }
            for ptr in ptrs {
                unsafe { fl.free(ptr) };
            }
        })
    });

    // Slab allocator
    group.bench_function("slab_alloc_free_1000", |b| {
        let slab = SlabAllocator::new(64, 256);
        b.iter(|| {
            let mut ptrs = Vec::with_capacity(1000);
            for _ in 0..1000 {
                ptrs.push(slab.alloc().unwrap());
            }
            for ptr in ptrs {
                unsafe { slab.free(ptr) };
            }
        })
    });

    group.finish();
}

fn bench_gc(c: &mut Criterion) {
    let mut group = c.benchmark_group("gc");

    group.bench_function("alloc_collect_100", |b| {
        b.iter(|| {
            let mut gc = GarbageCollector::new(1024 * 1024);
            for i in 0..100 {
                let ptr = gc.alloc(64, TypeTag::Blob).unwrap();
                if i % 3 == 0 {
                    gc.add_root(ptr.as_ptr());
                }
            }
            gc.collect();
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_lexer,
    bench_parser,
    bench_full_pipeline,
    bench_vm_execution,
    bench_allocators,
    bench_gc,
);
criterion_main!(benches);
