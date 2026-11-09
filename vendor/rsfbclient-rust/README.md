# Local rsfbclient-rust 0.29.0

Source: the published 0.29.0 crate from https://github.com/fernandobatels/rsfbclient.
Upstream commit recorded in the published crate: `71d84675c171f8b44bb5e960b4ba75953007e716`.
License: MIT (see LICENSE).

Local functional changes:

- ChaCha64 and ChaCha use RustCrypto chacha20 0.10.2, with SHA-256 key derivation and server IV/counter handling matching Firebird's native plugin. Protocol 16 adds cipher-specific data and execution timeout fields; protocol 13 remains for Firebird 3/Arc4. The fork targets Firebird 3+ and removes protocol 10–12 / legacy password fallback, including the unused pwhash dependency.
- A shared cipher stream handles short/interrupted writes and rejects reuse after a partial write failure. Tests exercise all three ciphers and ChaCha64's carry beyond the 32-bit block counter. WireCrypt policy and the Arc4 allow-list are explicit; the plugin defaults to required encryption.

- Caller-owned TCP sockets allow connect deadlines and cancellation during authentication; the plugin aborts its dedicated attachment by shutting down that socket. An optional shared cancellation signal also bounds receive waits to 100 ms, because Windows shutdown does not reliably wake blocked reads. Receive timeouts retry until data or cancellation; failed writes are not retried. The plugin detaches completed controls from that signal before parking a session and rebinds it for the next request.
- Configurable decoded BLOB limits and single-row prefetch for BLOB results bound their buffering. Affected-row metadata is exposed after execute2/RETURNING.
- DPB paths/user/role strings explicitly use UTF-8 independently of the connection character set. SQLDA names decode strictly using the connection character set instead of silently becoming empty names. Text BLOBs use BLOB2 descriptors and explicit source/target character sets when opening; long bound text is a UTF-8 text BLOB instead of an untyped binary BLOB. Binary bytes remain untouched, and custom BLOB subtypes are rejected.
- Prepared statements expose Firebird 3+ detailed plans through statement information item 26. The 65535-byte response is checked for truncation before decoding.

- `StmtHandleData::column_names()` exposes prepared output names, even for an empty cursor.
- Fixed-point NUMERIC/DECIMAL output is requested as VARCHAR(64) instead of DOUBLE, so Firebird converts it to exact decimal text before transmission. Ordinary FLOAT/DOUBLE remain numeric, and unscaled BIGINT/INT128 keep their integer decoding.
- DECFLOAT(16/34) is requested as VARCHAR(64), preserving digits, exponents and special values.
- DATE/TIME/TIMESTAMP, with or without time zone, are requested as VARCHAR(128), preserving their original kind, fractional seconds and zone. SQLDA codes missing from the upstream core bindings are defined locally with a reference to Firebird's public header.

Replace this copy with an upstream release when it exposes statement metadata, exact decimal values and faithful temporal values. No Firebird server/client binaries are included here.

Wire references: [Firebird ChaCha plugin](https://github.com/FirebirdSQL/firebird/blob/master/src/plugins/crypt/chacha/ChaCha.cpp), [server key extraction](https://github.com/FirebirdSQL/firebird/blob/master/src/remote/server/server.cpp), and the [wire protocol](https://firebirdsql.org/file/documentation/html/en/firebirddocs/wireprotocol/firebird-wire-protocol.html).

Text references: [native BLOB2 descriptors](https://github.com/FirebirdSQL/firebird/blob/master/src/remote/client/BlrFromMessage.cpp), [Jaybird BLOB parameter buffers](https://github.com/FirebirdSQL/jaybird/blob/master/src/main/org/firebirdsql/jdbc/FBBlob.java), and [UTF-8 DPB strings](https://github.com/FirebirdSQL/jaybird/blob/master/src/main/org/firebirdsql/gds/ng/wire/version13/V13ParameterConverter.java). Firebird applies the attachment character set before returning text; values outside that character set must produce an error, never replacement characters.

Coercion tests were updated and expanded for fixed-point, DECFLOAT and temporal types. Cargo lint settings recognize upstream's coverage configuration and allow its existing unused internal fields. Source formatting follows the workspace rustfmt settings.
