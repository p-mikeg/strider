# ARM in Thumb-2 mode.  Same toolchain as `arm.mk`, but `-mthumb` forces
# Thumb-2 encoding throughout.  The Sleigh `ARM8_le` spec + `ARMCORTEX`
# pspec decode Thumb-only.
CC     := $(shell command -v arm-linux-gnueabihf-gcc 2>/dev/null || echo false)
CFLAGS := -mthumb -O2 -g -fno-stack-protector
