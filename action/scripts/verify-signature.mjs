// Checks the Ed25519 signature of a release archive. The installer runs this
// script before the SHA-256 check. Exit 0: the signature is good. Exit 1: the
// signature is bad, missing or unreadable.
//
// The public key is the key of the fallow repo (npm/fallow/scripts/
// verify-binary.js). The key is in this file, so a change to the network path
// or to the release cannot change it. CARGO_FALLOW_SIGNING_PUBLIC_KEY (32 bytes
// in base64) replaces the key for the tests in action/tests/run.sh only.
//
// Usage: node verify-signature.mjs <file> <sig>

import { readFileSync } from "node:fs";
import { createPublicKey, verify } from "node:crypto";

/** The 32-byte Ed25519 public key of the fallow release signatures. */
const EMBEDDED_PUBLIC_KEY = Buffer.from([
  131, 78, 111, 215, 115, 51, 230, 238, 223, 119, 147, 71, 199, 16, 172, 180, 3, 210, 216, 35, 77,
  85, 159, 94, 215, 200, 126, 85, 42, 222, 11, 209,
]);
/** The SPKI DER prefix of an Ed25519 public key (RFC 8410). */
const SPKI_HEADER = Buffer.from([
  0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
]);
/** The length of an Ed25519 public key in bytes. */
const KEY_LENGTH = 32;
/** The length of an Ed25519 signature in bytes. */
const SIGNATURE_LENGTH = 64;

const reject = (message) => {
  console.error(message);
  process.exit(1);
};

const readOrReject = (path, what) => {
  try {
    return readFileSync(path);
  } catch (error) {
    return reject(`Could not read the ${what} ${path}: ${error.message}`);
  }
};

const [, , filePath, sigPath] = process.argv;
if (!filePath || !sigPath) {
  reject("usage: verify-signature.mjs <file> <sig>");
}

const override = process.env.CARGO_FALLOW_SIGNING_PUBLIC_KEY;
const rawKey = override ? Buffer.from(override.trim(), "base64") : EMBEDDED_PUBLIC_KEY;
if (rawKey.length !== KEY_LENGTH) {
  reject(`The public key has ${rawKey.length} bytes, not ${KEY_LENGTH}.`);
}

const data = readOrReject(filePath, "file");
const signature = readOrReject(sigPath, "signature");
if (signature.length !== SIGNATURE_LENGTH) {
  reject(`The signature ${sigPath} has ${signature.length} bytes, not ${SIGNATURE_LENGTH}.`);
}

const publicKey = createPublicKey({
  key: Buffer.concat([SPKI_HEADER, rawKey]),
  format: "der",
  type: "spki",
});
let valid = false;
try {
  valid = verify(null, data, publicKey, signature);
} catch (error) {
  reject(`The signature check failed: ${error.message}`);
}
if (!valid) {
  reject(`The Ed25519 signature of ${filePath} is not valid.`);
}
