# consumer-kit-v8

Picks `@solana/kit@^8` — the version the client is actually built against. The
failure is at **install** time, not build time: the client's declared peer
`@solana/kit@^6.10.0` rejects it.

```
$ corepack pnpm install
✓ Lockfile passes supply-chain policies (verified 28s ago)
Packages: +1
+
Progress: resolved 49, reused 49, downloaded 0, added 1, done
[WARN] Issues with peer dependencies found. Run "pnpm peers check" to list them.
Done in 2.3s using pnpm v11.25.0
$ corepack pnpm peers check
Issues with peer dependencies found

✕ unmet peer @solana/kit
  Installed: 8.2.0
  Wanted:
    ^6.10.0:
      cow-solana-settlement-client@
```

Under `--strict-peer-dependencies` (or npm) that warning becomes a hard failure.

There is **no failing build** here — that's the point. Once installed, the tree
has a single `@solana/addresses@8.2.0`, so the client compiles cleanly:

```
$ corepack pnpm why @solana/addresses
Found 1 version of @solana/addresses
$ corepack pnpm run build
$ echo $?
0
```

Contrast with `consumer-kit-v6`, which installs cleanly but then fails to build.
