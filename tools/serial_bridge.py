#!/usr/bin/env python3
"""Python bridge for QEMU interactive serial console.
Connects to QEMU's UNIX serial socket and bridges I/O to/from a FIFO and a log file.

Usage:
    python3 serial_bridge.py <build-dir> <socket-path> <fifo-path>

The bridge:
1. Connects to QEMU's serial unix socket at <socket-path>
2. Reads from the socket and writes to <build-dir>/serial.log
3. Reads from <fifo-path> and writes to the socket (sends commands to QEMU)
4. Exits when EOF is received from the socket or the FIFO
"""

import sys
import os
import socket
import time
import select

def main():
    if len(sys.argv) < 4:
        print("Usage: python3 serial_bridge.py <build-dir> <socket-path> <fifo-path>", file=sys.stderr)
        sys.exit(1)

    build_dir = sys.argv[1]
    socket_path = sys.argv[2]
    fifo_path = sys.argv[3]
    serial_log = os.path.join(build_dir, "serial.log")

    # Connect to QEMU's serial unix socket
    try:
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.connect(socket_path)
    except Exception as e:
        print(f"Failed to connect to serial socket {socket_path}: {e}", file=sys.stderr)
        sys.exit(1)

    # Open serial.log for appending
    log_fd = os.open(serial_log, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)

    # Open FIFO for reading
    try:
        fifo_fd = os.open(fifo_path, os.O_RDONLY | os.O_NONBLOCK)
    except OSError:
        print(f"Failed to open FIFO {fifo_path}", file=sys.stderr)
        sys.exit(1)

    print(f"Serial bridge started: socket->{socket_path}, log->{serial_log}, fifo->{fifo_path}")

    inputs = [sock, fifo_fd]
    fifo_closed = False

    try:
        while True:
            # Use select to handle both directions. FIFO EOF only stops writes;
            # the socket side keeps being logged until QEMU closes it.
            readable, _, _ = select.select(inputs, [], [], 1.0)

            if sock in readable:
                try:
                    data = sock.recv(4096)
                    if data:
                        os.write(log_fd, data)
                    else:
                        # Socket closed
                        break
                except (BlockingIOError, ConnectionError, OSError):
                    pass

            if fifo_fd in readable:
                try:
                    data = os.read(fifo_fd, 4096)
                    if data:
                        sock.sendall(data)
                    else:
                        # EOF on FIFO: stop watching it, keep logging the socket.
                        if not fifo_closed:
                            inputs.remove(fifo_fd)
                            fifo_closed = True
                except (BlockingIOError, ConnectionError, OSError):
                    pass

    except KeyboardInterrupt:
        pass
    finally:
        os.close(log_fd)
        os.close(fifo_fd)
        sock.close()
        print("\nSerial bridge terminated.")

if __name__ == "__main__":
    main()