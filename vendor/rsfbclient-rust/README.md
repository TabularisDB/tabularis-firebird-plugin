# Local rsfbclient-rust 0.29.0

Source: the published 0.29.0 crate from https://github.com/fernandobatels/rsfbclient.
Upstream commit recorded in the published crate: `71d84675c171f8b44bb5e960b4ba75953007e716`.
License: MIT (see LICENSE).

Local functional changes:

- `StmtHandleData::column_names()` exposes prepared output names, even for an empty cursor.
- Fixed-point NUMERIC/DECIMAL output is requested as VARCHAR(64) instead of DOUBLE, so Firebird converts it to exact decimal text before transmission. Ordinary FLOAT/DOUBLE remain numeric, and unscaled BIGINT/INT128 keep their integer decoding.
- DECFLOAT(16/34) is requested as VARCHAR(64), preserving digits, exponents and special values.
- DATE/TIME/TIMESTAMP, with or without time zone, are requested as VARCHAR(128), preserving their original kind, fractional seconds and zone. SQLDA codes missing from the upstream core bindings are defined locally with a reference to Firebird's public header.

Replace this copy with an upstream release when it exposes statement metadata, exact decimal values and faithful temporal values. No Firebird server/client binaries are included here.

Coercion tests were updated and expanded for fixed-point, DECFLOAT and temporal types. Cargo lint settings recognize upstream's coverage configuration and allow its existing unused internal fields. Source formatting follows the workspace rustfmt settings.
