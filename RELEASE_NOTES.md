# Firebird driver 0.1.0

Initial read-only Firebird driver for Tabularis. Connect to a local or remote Firebird server over TCP without installing a Firebird client DLL.

- Browse tables, columns, keys, indexes, views, top-level routines and ER relationships.
- Execute SELECT queries with pagination and Firebird SQL previews.
- Preserve exact decimals, large integers, temporal values and Firebird 4+ DECFLOAT/time-zone values.
- Tested with Firebird 3.0.14, 4.0.7 and 5.0.4 on Windows.
- Native release packages for Linux x64/ARM64, macOS x64/ARM64 and Windows x64 include the manifest and license notices.

In Tabularis, choose Firebird, configure host/port and credentials, then enter the server-side database path or alias on the Databases tab. The server must already exist. See README for installation, platform requirements and tested flows.

Writes, DDL, EXPLAIN, embedded access and TLS configuration are not implemented. Linux/macOS package smoke tests cover executable startup and JSON-RPC; live Firebird and Tabularis GUI validation was performed on Windows. Registry ingestion and final bounty acceptance require publication and maintainer review.
