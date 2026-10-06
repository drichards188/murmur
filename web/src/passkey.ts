/** Passkeys in the browser (rules 10): WebAuthn's `navigator.credentials` with the options the
 *  bridge gives, as JSON (base64url fields), and the response back as JSON for the bridge. The
 *  bridge derives each step-up's challenge from the act it authorizes (an engagement signature:
 *  the intent and the picture token you were shown), so an assertion signs that one act; this
 *  file only carries it. Nothing here is stored.
 *
 *  WebAuthn needs a domain: a passkey's relying party is `localhost` (the bridge's
 *  `MURMUR_PASSKEY_RP_ID`), and a page at http://127.0.0.1:<port> can't use it (the browser
 *  refuses an IP address). [`passkeyPlace`] tells which, and where to go instead. */

export interface PasskeyInfo {
  /** the relying party id (`localhost`) and the console origins on it */
  rp_id: string
  origins: string[]
  /** the signed-in operator's passkeys this run can use */
  registered?: number
}

function b64urlToBytes(s: string): Uint8Array<ArrayBuffer> {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4)
  const bin = atob(b64)
  const out = new Uint8Array(new ArrayBuffer(bin.length))
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

function bytesToB64url(buf: ArrayBuffer | null): string | null {
  if (buf == null) return null
  const bytes = new Uint8Array(buf)
  let bin = ''
  for (const b of bytes) bin += String.fromCharCode(b)
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

/** Whether this page can use the run's passkeys, and the console's address where it can. */
export function passkeyPlace(info: PasskeyInfo | undefined): { supported: boolean; here: boolean; url: string | null } {
  const supported = typeof window !== 'undefined' && 'PublicKeyCredential' in window && !!navigator.credentials
  if (!info) return { supported, here: false, url: null }
  const host = window.location.hostname
  const here = host === info.rp_id || host.endsWith(`.${info.rp_id}`)
  const port = window.location.port
  const same = info.origins.find((o) => o.endsWith(`:${port}`)) ?? info.origins[0] ?? null
  return { supported, here, url: same ? `${same}${window.location.pathname}${window.location.hash}` : null }
}

/** Why a passkey can't be used on this page, if it can't. */
export function passkeyBlocker(info: PasskeyInfo | undefined): string | null {
  const p = passkeyPlace(info)
  if (!info) return 'this run takes no passkey'
  if (!p.supported) return 'this browser has no WebAuthn (passkeys): use a current browser'
  if (!p.here) return `passkeys work at ${p.url ?? `http://${info.rp_id}`} (WebAuthn takes no IP address): open the console there`
  return null
}

function friendly(e: unknown, what: 'get' | 'create'): Error {
  const name = (e as { name?: string }).name
  if (name === 'NotAllowedError') {
    return new Error(
      what === 'get'
        ? 'no passkey answered: this browser has none of yours for this console, or the prompt was cancelled or timed out'
        : 'no passkey was made: the prompt was cancelled or timed out',
    )
  }
  if (name === 'InvalidStateError') return new Error('this authenticator already holds a passkey of yours here')
  if (name === 'SecurityError') return new Error('the browser refused passkeys on this address: open the console at http://localhost:<port>')
  return e instanceof Error ? e : new Error(String(e))
}

interface GetOptions {
  publicKey: {
    challenge: string
    timeout?: number
    rpId?: string
    allowCredentials?: { type: 'public-key'; id: string; transports?: string[] }[]
    userVerification?: UserVerificationRequirement
  }
}

/** Ask the authenticator to sign the bridge's challenge: the assertion, as JSON. */
export async function getAssertion(options: GetOptions): Promise<Record<string, unknown>> {
  const o = options.publicKey
  const publicKey: PublicKeyCredentialRequestOptions = {
    challenge: b64urlToBytes(o.challenge),
    timeout: o.timeout,
    rpId: o.rpId,
    userVerification: o.userVerification ?? 'required',
    allowCredentials: (o.allowCredentials ?? []).map((c) => ({
      type: 'public-key' as const,
      id: b64urlToBytes(c.id),
      transports: c.transports as AuthenticatorTransport[] | undefined,
    })),
  }
  let cred: PublicKeyCredential
  try {
    cred = (await navigator.credentials.get({ publicKey })) as PublicKeyCredential
  } catch (e) {
    throw friendly(e, 'get')
  }
  if (!cred) throw new Error('no passkey answered')
  const r = cred.response as AuthenticatorAssertionResponse
  return {
    id: cred.id,
    rawId: bytesToB64url(cred.rawId),
    type: cred.type,
    response: {
      authenticatorData: bytesToB64url(r.authenticatorData),
      clientDataJSON: bytesToB64url(r.clientDataJSON),
      signature: bytesToB64url(r.signature),
      userHandle: bytesToB64url(r.userHandle),
    },
    extensions: cred.getClientExtensionResults?.() ?? {},
  }
}

interface CreateOptions {
  publicKey: {
    rp: { id?: string; name: string }
    user: { id: string; name: string; displayName: string }
    challenge: string
    pubKeyCredParams: { type: 'public-key'; alg: number }[]
    timeout?: number
    excludeCredentials?: { type: 'public-key'; id: string; transports?: string[] }[]
    authenticatorSelection?: AuthenticatorSelectionCriteria
    attestation?: AttestationConveyancePreference
    extensions?: Record<string, unknown>
  }
}

/** Ask the authenticator to make a passkey for the bridge's options: the registration, as JSON. */
export async function createCredential(options: CreateOptions): Promise<Record<string, unknown>> {
  const o = options.publicKey
  const publicKey: PublicKeyCredentialCreationOptions = {
    rp: o.rp,
    user: { id: b64urlToBytes(o.user.id), name: o.user.name, displayName: o.user.displayName },
    challenge: b64urlToBytes(o.challenge),
    pubKeyCredParams: o.pubKeyCredParams,
    timeout: o.timeout,
    excludeCredentials: (o.excludeCredentials ?? []).map((c) => ({
      type: 'public-key' as const,
      id: b64urlToBytes(c.id),
      transports: c.transports as AuthenticatorTransport[] | undefined,
    })),
    authenticatorSelection: o.authenticatorSelection,
    attestation: o.attestation ?? 'none',
  }
  let cred: PublicKeyCredential
  try {
    cred = (await navigator.credentials.create({ publicKey })) as PublicKeyCredential
  } catch (e) {
    throw friendly(e, 'create')
  }
  if (!cred) throw new Error('no passkey was made')
  const r = cred.response as AuthenticatorAttestationResponse
  return {
    id: cred.id,
    rawId: bytesToB64url(cred.rawId),
    type: cred.type,
    response: {
      attestationObject: bytesToB64url(r.attestationObject),
      clientDataJSON: bytesToB64url(r.clientDataJSON),
      transports: r.getTransports?.() ?? [],
    },
    extensions: cred.getClientExtensionResults?.() ?? {},
  }
}
