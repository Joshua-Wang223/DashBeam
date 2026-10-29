#!/usr/bin/env node

// Verify that the updater signing key available to CI matches the public key
// baked into the app (src-tauri/tauri.conf.json -> plugins.updater.pubkey).
//
// The Tauri CLI signs updater artifacts with whatever TAURI_SIGNING_PRIVATE_KEY
// it is given and never checks it against the configured pubkey, so a stale or
// wrong secret produces a green build whose .sig files every installed client
// rejects. This check signs a canary file with the CI key and verifies the
// resulting signature against the configured public key, so a mismatch is
// reported before the release is published.

import { execFileSync } from 'node:child_process'
import crypto from 'node:crypto'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __filename = fileURLToPath(import.meta.url)
const __dirname = path.dirname(__filename)
const root = path.join(__dirname, '..')
const tauriConfigPath = path.join(root, 'src-tauri', 'tauri.conf.json')
const tauriCliPath = path.join(
	root,
	'node_modules',
	'@tauri-apps',
	'cli',
	'tauri.js'
)

// Minisign blobs are `alg(2) || key_id(8) || payload`. The payload is the
// 32-byte Ed25519 public key for a public key blob and the 64-byte signature
// for a signature blob.
const KEY_ID_OFFSET = 2
const KEY_ID_LENGTH = 8
const PUBKEY_OFFSET = 10
const PUBKEY_LENGTH = 32
const SIGNATURE_OFFSET = 10
const SIGNATURE_LENGTH = 64
const ED25519_SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex')

class VerificationError extends Error {}

function decodeBlob(base64Body, what) {
	const decoded = Buffer.from(base64Body, 'base64')
	if (decoded.length < KEY_ID_OFFSET + KEY_ID_LENGTH) {
		throw new VerificationError(
			`${what} is malformed (${decoded.length} bytes)`
		)
	}
	return decoded
}

function parsePublicKey(pubkeyB64) {
	const outer = Buffer.from(pubkeyB64, 'base64').toString()
	const lines = outer.split(/\r?\n/)
	const body = lines.find(
		(line) => line && !line.startsWith('untrusted comment:')
	)
	if (!body) {
		throw new VerificationError('configured pubkey does not contain a key body')
	}
	const blob = decodeBlob(body, 'configured pubkey')
	return {
		keyId: blob
			.subarray(KEY_ID_OFFSET, KEY_ID_OFFSET + KEY_ID_LENGTH)
			.toString('hex')
			.toUpperCase(),
		publicKey: blob.subarray(PUBKEY_OFFSET, PUBKEY_OFFSET + PUBKEY_LENGTH),
	}
}

function parseSignature(sigB64) {
	const outer = Buffer.from(sigB64, 'base64').toString()
	const lines = outer.split(/\r?\n/)
	const body = lines.find(
		(line) => line && !line.startsWith('untrusted comment:')
	)
	if (!body) {
		throw new VerificationError(
			'canary signature does not contain a signature body'
		)
	}
	const blob = decodeBlob(body, 'canary signature')
	return {
		keyId: blob
			.subarray(KEY_ID_OFFSET, KEY_ID_OFFSET + KEY_ID_LENGTH)
			.toString('hex')
			.toUpperCase(),
		signature: blob.subarray(
			SIGNATURE_OFFSET,
			SIGNATURE_OFFSET + SIGNATURE_LENGTH
		),
	}
}

// Minisign signs the BLAKE2b-512 digest of the payload with Ed25519.
function verifySignature(publicKey, payload, signature) {
	const digest = crypto.createHash('blake2b512').update(payload).digest()
	const spki = Buffer.concat([ED25519_SPKI_PREFIX, publicKey])
	const key = crypto.createPublicKey({
		key: spki,
		format: 'der',
		type: 'spki',
	})
	return crypto.verify(null, digest, key, signature)
}

function readPrivateKey() {
	if (process.env.TAURI_SIGNING_PRIVATE_KEY_PATH) {
		const keyPath = process.env.TAURI_SIGNING_PRIVATE_KEY_PATH
		if (!fs.existsSync(keyPath)) {
			throw new VerificationError(
				`TAURI_SIGNING_PRIVATE_KEY_PATH points to a missing file: ${keyPath}`
			)
		}
		return fs.readFileSync(keyPath, 'utf8').trim()
	}
	if (process.env.TAURI_SIGNING_PRIVATE_KEY) {
		return process.env.TAURI_SIGNING_PRIVATE_KEY.trim()
	}
	return null
}

function signCanary(privateKey) {
	const workDir = fs.mkdtempSync(path.join(os.tmpdir(), 'dashbeam-signing-'))
	const canaryPath = path.join(workDir, 'canary.txt')
	const payload = 'dashbeam updater signing check\n'
	try {
		fs.writeFileSync(canaryPath, payload)
		const env = { ...process.env }
		env.TAURI_SIGNING_PRIVATE_KEY = privateKey
		// Always define the password (empty when the key has none): the CLI
		// otherwise prompts on the terminal and blocks a non-interactive CI run.
		env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD =
			process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? ''
		try {
			execFileSync(
				process.execPath,
				[tauriCliPath, 'signer', 'sign', canaryPath],
				{ env, stdio: 'pipe', encoding: 'utf8', stdin: 'ignore' }
			)
		} catch (error) {
			const stderr = (error.stderr ?? '').trim()
			throw new VerificationError(
				'could not sign the canary file with the provided private key. ' +
					`Check that the key and its password are valid.\n${stderr}`
			)
		}
		const sigPath = `${canaryPath}.sig`
		if (!fs.existsSync(sigPath)) {
			throw new VerificationError(
				'tauri signer sign did not produce a .sig file'
			)
		}
		return {
			signature: parseSignature(fs.readFileSync(sigPath, 'utf8').trim()),
			payload: Buffer.from(payload),
		}
	} finally {
		fs.rmSync(workDir, { recursive: true, force: true })
	}
}

function main() {
	if (!fs.existsSync(tauriConfigPath)) {
		throw new VerificationError(`tauri config not found at ${tauriConfigPath}`)
	}
	const tauriConfig = JSON.parse(fs.readFileSync(tauriConfigPath, 'utf8'))
	const updater = tauriConfig.plugins?.updater
	if (!updater?.pubkey) {
		throw new VerificationError(
			'src-tauri/tauri.conf.json has no plugins.updater.pubkey to verify against'
		)
	}
	const privateKey = readPrivateKey()
	if (!privateKey) {
		throw new VerificationError(
			'tauri.conf.json declares an updater pubkey but no signing key was provided ' +
				'(set TAURI_SIGNING_PRIVATE_KEY or TAURI_SIGNING_PRIVATE_KEY_PATH)'
		)
	}
	if (!fs.existsSync(tauriCliPath)) {
		throw new VerificationError(
			`tauri CLI not found at ${tauriCliPath}; run pnpm install first`
		)
	}

	const { signature, payload } = signCanary(privateKey)
	const publicKey = parsePublicKey(updater.pubkey)

	console.log(`configured pubkey key id: ${publicKey.keyId}`)
	console.log(`CI signing key key id:    ${signature.keyId}`)

	if (publicKey.keyId !== signature.keyId) {
		throw new VerificationError(
			'the CI signing key does not match the updater pubkey in ' +
				'src-tauri/tauri.conf.json. Every updater signature produced by this ' +
				'build would be rejected by installed clients. Update the ' +
				'TAURI_SIGNING_PRIVATE_KEY secret to the matching private key, or ' +
				'set plugins.updater.pubkey to the public key of the key in use.'
		)
	}

	if (!verifySignature(publicKey.publicKey, payload, signature.signature)) {
		throw new VerificationError(
			'the canary signature produced by the CI signing key does not verify ' +
				'against the configured updater pubkey'
		)
	}

	console.log('updater signing key matches the configured pubkey')
}

try {
	main()
} catch (error) {
	if (error instanceof VerificationError) {
		console.error(`Error: ${error.message}`)
		process.exitCode = 1
	} else {
		console.error(error)
		process.exitCode = 1
	}
}
