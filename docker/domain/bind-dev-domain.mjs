import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { sign, createPublicKey } from 'node:crypto';
import { hashPassword } from 'file:///opt/buckyos/libexec/buckyos-tool/dist/node.mjs';
import { getDevTestKeyPairById } from 'file:///opt/buckyos/libexec/buckyos-tool/dist/provision.mjs';

const [mode, domain] = process.argv.slice(2);
if (!['--prepare', '--apply'].includes(mode) || !domain ||
    !/^(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$/.test(domain)) {
  throw new Error('Usage: bind-dev-domain.mjs --prepare|--apply <hostname>; devtest Zone only');
}
const backup = `/tmp/domain-binding-${Date.now()}`;
mkdirSync(backup, { recursive: true, mode: 0o700 });
const password = readFileSync(0, 'utf8').trim();
const nonce = Date.now();
async function rpc(service, method, params, token) {
  const response = await fetch(`http://127.0.0.1:3180/kapi/${service}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Host: 'test.buckyos.io' },
    body: JSON.stringify({ method, params, sys: token ? [nonce, token] : [nonce] }),
    signal: AbortSignal.timeout(15000),
  });
  const data = await response.json();
  if (!response.ok || data.error || !('result' in data)) {
    throw new Error(`${service}.${method}: ${JSON.stringify(data.error ?? data)}`);
  }
  return data.result;
}
const login = await rpc('verify-hub', 'sudo_by_password', {
  username: 'devtest', password: hashPassword('devtest', password, nonce),
  target: { kind: 'system', service_id: 'control-panel' },
  login_nonce: nonce,
});
const token = login.session_token;
const previous = await rpc('system_config', 'sys_config_get', { key: 'boot/config' }, token);
writeFileSync(`${backup}/boot-before.json`, JSON.stringify(previous, null, 2), { mode: 0o600 });
const config = JSON.parse(previous.value);
const document = config.zone_document.startsWith('{')
  ? JSON.parse(config.zone_document)
  : JSON.parse(Buffer.from(config.zone_document.split('.')[1], 'base64url').toString());
const key = getDevTestKeyPairById('devtest');
const identity = JSON.parse(readFileSync('/opt/buckyos/etc/node_identity.json', 'utf8'));
if (key.publicKeyX !== identity.owner_public_key.x || document.id !== identity.zone_did || document.id !== 'did:web:test.buckyos.io') {
  throw new Error('Owner key or Zone identity does not match the live installation');
}
document.hostname = domain;
document.iat = Math.max(document.iat + 1, Math.floor(Date.now() / 1000));
document.version_seq = (document.version_seq ?? 0) + 1;
for (const service of document.service ?? []) {
  if (service.type === 'DIDDoc') service.serviceEndpoint = `https://${domain}/resolve/this_zone`;
}
const header = Buffer.from(JSON.stringify({ alg: 'EdDSA' })).toString('base64url');
const payload = Buffer.from(JSON.stringify(document)).toString('base64url');
const signature = sign(null, Buffer.from(`${header}.${payload}`), key.privateKeyPem).toString('base64url');
const signed = `${header}.${payload}.${signature}`;
const publicKey = createPublicKey(key.privateKeyPem).export({ format: 'jwk' });
if (publicKey.x !== identity.owner_public_key.x) throw new Error('Signing key mismatch');
config.zone_document = signed;
writeFileSync(`${backup}/boot-after.json`, JSON.stringify(config, null, 2), { mode: 0o600 });
writeFileSync(`${backup}/zone-document.jwt`, signed, { mode: 0o600 });
if (mode === '--apply') {
  await rpc('system_config', 'sys_config_set', { key: 'boot/config', value: JSON.stringify(config) }, token);
  writeFileSync('/opt/buckyos/etc/test.buckyos.io.zone.json', signed + '\n');
  writeFileSync('/opt/buckyos/etc/zone_document.jwt', signed + '\n');
  writeFileSync('/opt/buckyos/etc/.buckycli/zone_config.json', JSON.stringify(document, null, 2) + '\n');
  const start = JSON.parse(readFileSync('/opt/buckyos/etc/start_config.json', 'utf8'));
  start.access_hostname = domain;
  start.zone_document_jwt = signed;
  writeFileSync('/opt/buckyos/etc/start_config.json', JSON.stringify(start, null, 2) + '\n');
  const readback = await rpc('system_config', 'sys_config_get', { key: 'boot/config' }, token);
  if (JSON.parse(readback.value).zone_document !== signed) throw new Error('Domain binding readback mismatch');
}
console.log(JSON.stringify({ mode: mode ?? 'prepare', hostname: document.hostname, zone_did: document.id, owner_key_verified: true, backup } ));
