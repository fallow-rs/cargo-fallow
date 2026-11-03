// Signs a release archive with Ed25519, the same model as the fallow repo.
//
// ED25519_BINARY_SIGNING_PRIVATE_KEY holds the 32-byte Ed25519 seed in
// base64, not a PKCS8 document. The fallow repo uses the same secret format, with its own key.
//
// Usage: node sign-binary.mjs <input-file> <output-sig>

import { readFileSync, writeFileSync } from "node:fs";
import { createPrivateKey, sign } from "node:crypto";

/** The length of an Ed25519 seed in bytes. */
const SEED_LENGTH = 32;
/** The length of an Ed25519 signature in bytes. */
const SIGNATURE_LENGTH = 64;
/** The PKCS8 DER prefix of an Ed25519 private key (RFC 8410). */
const PKCS8_HEADER = Buffer.from([
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
]);

const [, , inputPath, outputPath] = process.argv;
if (!inputPath || !outputPath) {
  console.error("usage: sign-binary.mjs <input-file> <output-sig>");
  process.exit(1);
}

const seedBase64 = process.env.ED25519_BINARY_SIGNING_PRIVATE_KEY;
if (!seedBase64) {
  console.error("The ED25519_BINARY_SIGNING_PRIVATE_KEY variable is empty.");
  process.exit(1);
}

const seed = Buffer.from(seedBase64.trim(), "base64");
if (seed.length !== SEED_LENGTH) {
  console.error(`The seed has ${seed.length} bytes. An Ed25519 seed has ${SEED_LENGTH} bytes.`);
  process.exit(1);
}

const privateKey = createPrivateKey({
  key: Buffer.concat([PKCS8_HEADER, seed]),
  format: "der",
  type: "pkcs8",
});
const signature = sign(null, readFileSync(inputPath), privateKey);
if (signature.length !== SIGNATURE_LENGTH) {
  console.error(`The signature has ${signature.length} bytes, not ${SIGNATURE_LENGTH}.`);
  process.exit(1);
}

writeFileSync(outputPath, signature);
console.log(`Wrote the signature of ${inputPath} to ${outputPath}.`);
