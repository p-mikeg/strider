/* A lazily bound call through the PLT, so the loader writes the GOT header
 * words its resolver stub reads. */
extern int ext(void);
int call_ext(void) { return ext() + 1; }
