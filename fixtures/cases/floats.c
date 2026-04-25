/*
 * floats.c — F32 / F64 arithmetic, comparisons, and conversions.
 */
#define NOINLINE __attribute__((noinline))

float  NOINLINE f32_arith(float a, float b)  { return ((a + b) * a - b) / (a + 1.0f); }
double NOINLINE f64_arith(double a, double b){ return ((a + b) * a - b) / (a + 1.0);  }

double NOINLINE f32_to_f64(float a)  { return (double)a; }
float  NOINLINE f64_to_f32(double a) { return (float)a;  }
float  NOINLINE int_to_float(int a)  { return (float)a;  }
int    NOINLINE float_to_int(float a){ return (int)a;    }

int NOINLINE f32_compare(float a, float b) {
    if (a < b) return -1;
    if (a > b) return  1;
    return 0;
}
int NOINLINE f64_compare(double a, double b) {
    if (a < b) return -1;
    if (a > b) return  1;
    return 0;
}

float NOINLINE f32_neg_abs(float a) { return -a; }

int main(void) {
    volatile int s = 0;
    s ^= (int)f32_arith(1.5f, 2.5f);
    s ^= (int)f64_arith(1.5,  2.5);
    s ^= (int)f32_to_f64(1.5f);
    s ^= (int)f64_to_f32(1.5);
    s ^= (int)int_to_float(7);
    s ^= float_to_int(7.5f);
    s ^= f32_compare(1.0f, 2.0f);
    s ^= f64_compare(1.0,  2.0);
    s ^= (int)f32_neg_abs(-3.5f);
    return s;
}
