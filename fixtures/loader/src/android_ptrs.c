/* `ptrs`, a relro table of three local pointers and an import: three
 * R_AARCH64_RELATIVE and one R_AARCH64_ABS64, which lld's
 * `--pack-dyn-relocs=android` packs into an SHT_ANDROID_RELA table. */
extern int imp(void);
static int g1(void) { return 1; }
static int g2(void) { return 2; }
__attribute__((visibility("hidden"))) int (*const ptrs[4])(void) = {g1, g2, imp, g1};
