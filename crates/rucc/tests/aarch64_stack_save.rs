//! A function holding a variable length array saves the stack pointer with a copy into a
//! register. On AArch64 that copy has to stay a copy, because register thirty one in a store is
//! the zero register and not the stack pointer, so `str sp, [x29, #-88]` is not an instruction.
//! The pass that reads a scratch register's copy out of the register it came from turned the
//! store of the saved value into exactly that, and the encoder refused it. This is
//! `frequency_plot` from LTP's `tst_timer_test.c`, which needs the array and enough pressure that
//! the saved value is spilled.

use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = r#"typedef unsigned long size_t;
typedef struct FILE FILE;
extern FILE *stderr;
int fputc(int, FILE *);
int fprintf(FILE *, const char *, ...);
size_t strlen(const char *);
void *memset(void *, int, size_t);
#define MAX(a, b) ((a) > (b) ? (a) : (b))
static long long *samples;
static unsigned int cur_sample;
static void print_line(char c, int len) { while (len-- > 0) fputc(c, stderr); }
static unsigned int ceilu(float f) { if (f - (int)f > 0) return (unsigned int)f + 1; return (unsigned int)f; }
static unsigned int flooru(float f) { return (unsigned int)f; }
static float bucket_len(unsigned int bucket, unsigned int max_bucket, unsigned int cols) { return 1.00 * bucket * cols / max_bucket; }
static const char *table_heading = " Time: us ";
static unsigned int header_len(long long max_sample) { size_t l = 1; while (max_sample/=10) l++; return MAX(strlen(table_heading) + 2, l + 3); }
void frequency_plot(void)
{
	unsigned int cols = 80;
	unsigned int rows = 20;
	unsigned int i, buckets[rows];
	long long max_sample = samples[0];
	long long min_sample = samples[cur_sample-1];
	unsigned int line_header_len = header_len(max_sample);
	unsigned int plot_line_len = cols - line_header_len;
	unsigned int bucket_size;
	memset(buckets, 0, sizeof(buckets));
	bucket_size = MAX(1u, ceilu(1.00 * (max_sample - min_sample)/(rows-1)));
	for (i = 0; i < cur_sample; i++) {
		unsigned int bucket;
		bucket = flooru(1.00 * (samples[i] - min_sample)/bucket_size);
		buckets[bucket]++;
	}
	unsigned int max_bucket = buckets[0];
	for (i = 1; i < rows; i++)
		max_bucket = MAX(max_bucket, buckets[i]);
	fprintf(stderr, "\n%*s| Frequency\n", line_header_len - 2, table_heading);
	print_line('-', cols);
	fputc('\n', stderr);
	unsigned int l, r;
	for (l = 0; l < rows; l++) { if (buckets[l]) break; }
	for (r = rows-1; r > l; r--) { if (buckets[r]) break; }
	for (i = l; i <= r; i++) {
		float len = bucket_len(buckets[i], max_bucket, plot_line_len);
		fprintf(stderr, "%*lli | ", line_header_len - 3, min_sample + bucket_size*i);
		print_line('*', len);
		if ((len - (int)len) >= 0.5) fputc('+', stderr);
		else if ((len - (int)len) >= 0.25) fputc('-', stderr);
		else if (len < 0.25 && buckets[i]) fputc('.', stderr);
		fputc('\n', stderr);
	}
	print_line('-', cols);
	fputc('\n', stderr);
	float scale = 1.00 * plot_line_len / max_bucket;
	fprintf(stderr, "%*uus | 1 sample = %.5f '*', %.5f '+', %.5f '-', non-zero '.'\n", line_header_len - 5, bucket_size, scale, scale * 2, scale * 4);
	fputc('\n', stderr);
}
"#;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-a64-stack-save-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

#[test]
fn a_saved_stack_pointer_that_is_spilled_is_copied_out_first() {
    let dir = dir("spilled");
    std::fs::write(dir.join("a.c"), SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-S", "-O2", "-o", "-", "a.c"])
        .current_dir(Path::new(&dir))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{err}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.lines().any(|line| line.trim().starts_with("mov x") && line.ends_with(", sp")),
        "{text}"
    );
    assert!(!text.lines().any(|line| line.trim().starts_with("str sp")), "{text}");
}
