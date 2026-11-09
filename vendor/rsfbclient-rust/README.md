# Local rsfbclient-rust 0.29.0

Source: the published 0.29.0 crate from https://github.com/fernandobatels/rsfbclient.
Upstream commit recorded in the published crate: `71d84675c171f8b44bb5e960b4ba75953007e716`.
License: MIT (see LICENSE).

Two local functional changes:

- `StmtHandleData::column_names()` exposes prepared output names, even for an empty cursor.
- Fixed-point NUMERIC/DECIMAL output is requested as VARCHAR(64) instead of DOUBLE, so Firebird converts it to exact decimal text before transmission. Ordinary FLOAT/DOUBLE remain numeric, and unscaled BIGINT/INT128 keep their integer decoding.

Replace this copy with an upstream release when it exposes statement metadata and exact fixed-point values. No Firebird server/client binaries are included here.

The fixed-point coercion tests were updated and expanded. Cargo lint settings recognize upstream's coverage configuration and allow its existing unused internal fields. Source formatting follows the workspace rustfmt settings.
