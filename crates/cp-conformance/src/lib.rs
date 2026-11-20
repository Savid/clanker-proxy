//! Black-box tests of clanker-proxy daemons over HTTP; see `tests/`. They
//! run two native daemons, or, with `CP_CONFORMANCE_URL` and
//! `CP_CONFORMANCE_TOKEN` set, one native daemon against the daemon at that
//! URL, such as `cpd-worker` under `wrangler dev`.
