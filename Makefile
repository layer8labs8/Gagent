# G AGENT (v3.1) - Pure C Build System (Zero Rust Dependency)
CC      ?= gcc
CFLAGS  := -O2 -Wall -Wextra -Werror -std=c99 -Isrc
SOURCES := src/error.c src/session.c src/bootstrap.c src/sandbox.c src/auth.c src/workspace.c src/tools.c src/engine.c src/main.c

.PHONY: all test clean

all: gagent_runtime

gagent_runtime: $(SOURCES) src/gagent.h
	$(CC) $(CFLAGS) $(SOURCES) -o gagent_runtime

test: gagent_runtime
	./gagent_runtime

clean:
	rm -f gagent_runtime *.o
