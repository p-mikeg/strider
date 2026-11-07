/* Imported TLS through an initial-exec slot (a TP offset) and a
 * general-dynamic one (a module id and offset, or under the descriptor
 * dialect a TLS descriptor): each a `.got` slot only the loader fills. */
extern __thread int ie __attribute__((tls_model("initial-exec")));
extern __thread int gd __attribute__((tls_model("global-dynamic")));
int get_ie(void) { return ie; }
int get_gd(void) { return gd; }
