use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benches/fixtures")
}

struct TestCase {
    name: &'static str,
    file: &'static str,
}

fn test_cases() -> Vec<TestCase> {
    vec![
        TestCase {
            name: "overlay",
            file: "frame_19.png",
        },
        TestCase {
            name: "overlay_bw",
            file: "frame_19_bw.png",
        },
        TestCase {
            name: "overlay2",
            file: "frame_262.png",
        },
        TestCase {
            name: "overlay2_bw",
            file: "frame_262_bw.png",
        },
        TestCase {
            name: "no_overlay",
            file: "frame_100.png",
        },
        TestCase {
            name: "no_overlay_bw",
            file: "frame_100_bw.png",
        },
    ]
}

fn psm_variants() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        ("default", None),
        ("psm6", Some("6")),
        ("psm11", Some("11")),
    ]
}

fn bench_subprocess_ocr(c: &mut Criterion) {
    let fixtures = fixtures_dir();
    let mut group = c.benchmark_group("subprocess_tesseract");
    group.sample_size(10);

    for case in test_cases() {
        let image_path = fixtures.join(case.file);
        let image_str = image_path.to_str().unwrap().to_string();

        for (psm_name, psm_value) in psm_variants() {
            let id = BenchmarkId::new(case.name, psm_name);
            group.bench_with_input(id, &(&image_str, psm_value), |b, (path, psm)| {
                b.iter(|| {
                    live_set_splitter::ocr::run_tesseract_ocr(path, *psm).unwrap();
                });
            });
        }
    }
    group.finish();
}

#[cfg(feature = "leptess-ocr")]
fn bench_leptess_ocr(c: &mut Criterion) {
    let fixtures = fixtures_dir();
    let mut group = c.benchmark_group("leptess_tesseract");
    group.sample_size(10);

    for case in test_cases() {
        let image_path = fixtures.join(case.file);
        let image_str = image_path.to_str().unwrap().to_string();

        for (psm_name, psm_value) in psm_variants() {
            let id = BenchmarkId::new(case.name, psm_name);

            let mut lt =
                live_set_splitter::ocr_leptess::create_tesseract_instance(psm_value).unwrap();

            group.bench_with_input(id, &image_str, |b, path| {
                b.iter(|| {
                    live_set_splitter::ocr_leptess::run_ocr(&mut lt, path).unwrap();
                });
            });
        }
    }
    group.finish();
}

#[cfg(feature = "leptess-ocr")]
fn bench_leptess_ocr_fresh_instance(c: &mut Criterion) {
    let fixtures = fixtures_dir();
    let mut group = c.benchmark_group("leptess_fresh_instance");
    group.sample_size(10);

    for case in test_cases() {
        let image_path = fixtures.join(case.file);
        let image_str = image_path.to_str().unwrap().to_string();

        for (psm_name, psm_value) in psm_variants() {
            let id = BenchmarkId::new(case.name, psm_name);
            group.bench_with_input(id, &(&image_str, psm_value), |b, (path, psm)| {
                b.iter(|| {
                    let mut lt =
                        live_set_splitter::ocr_leptess::create_tesseract_instance(*psm).unwrap();
                    live_set_splitter::ocr_leptess::run_ocr(&mut lt, path).unwrap();
                });
            });
        }
    }
    group.finish();
}

#[cfg(feature = "leptess-ocr")]
criterion_group!(
    benches,
    bench_subprocess_ocr,
    bench_leptess_ocr,
    bench_leptess_ocr_fresh_instance,
);

#[cfg(not(feature = "leptess-ocr"))]
criterion_group!(benches, bench_subprocess_ocr,);

criterion_main!(benches);
