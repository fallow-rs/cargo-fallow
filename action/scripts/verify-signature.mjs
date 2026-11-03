// Checks the Ed25519 signature of a release archive. The installer runs this
// script before the SHA-256 check. Exit 0: the signature is good. Exit 1: the
// signature is bad, missing or unreadable.
//
// The public key is the key of the fallow repo (npm/fallow/scripts/
// verify-binary.js). The key is in this file, so a change to the network path,
// to the release or to the environment cannot change it. The tests in
// action/tests/run.sh use a copy of this file with a test key.
//
// Usage: node verify-signature.mjs <file> <sig>

import { readFileSync } from "node:fs";
import { createPublicKey, verify } from "node:crypto";

/** The 32-byte Ed25519 public key of the fallow release signatures, in base64. */
const EMBEDDED_PUBLIC_KEY = "g05v13Mz5u7fd5NHxxCstAPS2CNNVZ9e18h+VSreC9E=";
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

const rawKey = Buffer.from(EMBEDDED_PUBLIC_KEY, "base64");
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
