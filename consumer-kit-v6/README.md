# consumer-kit-v6

Obeys the client's declared peer range (`@solana/kit@^6.10.0`). The install is
green and silent — the client is really a v8 package, so the breakage only shows
up when you build (type-check) against it.

```
$ corepack pnpm install
Packages: +77
Done in 2.6s using pnpm v11.25.0

$ corepack pnpm why @solana/addresses
Found 2 versions of @solana/addresses

$ corepack pnpm run build
node_modules/.../cow-solana-settlement-client/src/generated/programs/cowSettlement.ts(406,47): error TS2345: Argument of type 'T' is not assignable to parameter of type 'ClientWithRpc<GetAccountInfoApi & GetMultipleAccountsApi>'.
  Type '((address: import(".../@solana+addresses@6.10.0/.../address").Address, ...) => ...' is not assignable to type '((address: import(".../@solana+addresses@8.2.0/.../address").Address, ...) => ...'.
$ echo $?
2
```

`@solana/addresses@6.10.0` (from this consumer's `@solana/kit@6`) and
`@solana/addresses@8.2.0` (pulled in by the client via
`@solana/program-client-core@8` → `@solana/accounts@8` → …) both land in the
tree. They define the same branded `Address` type twice, so the two copies are
mutually unassignable and the build fails. The install that succeeds is the one
that cannot compile — and it only tells you at build time, not at install time.
