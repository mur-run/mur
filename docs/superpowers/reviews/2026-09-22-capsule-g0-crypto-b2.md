<!--
來源：fleet deep-research，run fleet-deep-research-01a0c9bc-0a0f-7a50-8eba-b128f6acd4e9
產出於 2026-09-22，1 iteration 收斂，3 worker。
fleet 自身未寫檔（log 末行：「awaiting file write」），本檔由 MUR 從 fleet_run.log 第 144–343 行原樣擷取。
評審對象 commit cf12d308；其後 f582e363 只改 §2.4／§2.1（B1-Q1 裁決），不在 B2 評審範圍內。
評審者受「僅網路研究、不讀本地檔案」約束，章節引用為範圍描述而非逐字核對——引用需自行對照契約正文。
-->

# Cryptographic Review: Capsule G0 Envelope — Findings Report

**Reviewed:** Commit cf12d308, branch docs/capsule-g0-contracts-r2  
**Spec Location:** /Volumes/Firecuda4tb/Projects/mur/docs/superpowers/specs/2026-09-22-capsule-g0-contracts.md (§4 envelope, §2 state spaces, §7 threat model)  
**Scope:** Primitive choices, KDF info strings, nonce derivation, numeric limits (FROZEN: field layout, Header0 cleartext carrying ordered_sources, AAD binding topology)  
**Review Method:** Web research on six focus points with independent authoritative sources  

---

## Executive Summary

The envelope design uses sound architectural patterns (AAD binding to full Header0, multi-layer HKDF, cleartext dependency graph for anti-transplant) consistent with IETF AEAD and RFC 5869 principles. **Three issues require clarification or parameter adjustment:**

1. **DEK as HKDF input material** — semantically coherent only if DEK is treated as uniformly random; requires explicit randomness assumption in spec.
2. **Body digest redundancy** — separate content digest is NOT redundant with AEAD tag; serves keyless verification role. No conflict found, but collision/truncation bounds must be stated.
3. **Nonce derivation across re-wraps** — salt reuse pattern requires explicit guarantee of new (salt, info) pair per wrapping operation to avoid catastrophic collision.

---

## CRITICAL FINDINGS

### 1. HKDF(DEK, ...) Semantics — DEK Input Keying Material Coherence

**Issue:** Using a DEK as HKDF input keying material requires explicit assumption that DEK is uniformly random.

**Authority:**
- **RFC 5869, Section 3 & Dhole Moments blog** (https://soatok.blog/2021/11/17/understanding-hkdf/): "The most common use-case of HKDF is to implement key-splitting, where a single input key (the Initial Keying Material, or IKM) is used to derive two or more independent keys."
- **StackExchange (Crypto) on HKDF salt** (https://crypto.stackexchange.com/questions/98479/): "It may prevent the derivation of the same keying material for different contexts (when the same input key material is used in such different contexts)."

**Findings:**
- HKDF's security assumes IKM is either high-entropy random or extracted from a noisy source (via HKDF-Extract). 
- If DEK is already uniformly random (e.g., from /dev/urandom or KMS), it is suitable as IKM directly.
- **If DEK is derived from a weak source (user password, partial entropy), HKDF-Extract MUST run first**, or HKDF will not provide the extraction step's domain separation.
- The spec must state DEK's source explicitly: "DEK is uniformly random, ≥256 bits" or "DEK is subject to HKDF-Extract before use as IKM."

**Verdict:** IMPORTANT — **Clarify DEK source and randomness assumption.** Current pattern is sound if DEK ∼ Uniform(256 bits), incoherent if DEK is extracted/password-derived without stated HKDF-Extract step.

---

### 2. ordered_sources ↔ layer_index Binding — Layer Reorder Prevention

**Issue:** Is the correspondence between ordered_sources entries and ciphertext layers actually pinned, or can a layer be transplanted to a different source entry?

**Authority:**
- **RFC 9771 (AEAD Properties)** (https://datatracker.ietf.org/doc/rfc9771/): "Authenticated encryption with associated data (AEAD) encrypts plaintext with a secret key, nonce, and associated data, producing a ciphertext and authentication tag."
- **Rogaway, Authenticated-Encryption with Associated-Data** (https://web.cs.ucdavis.edu/~rogaway/papers/ad.pdf): "The need to handle associated-data when using an integrated AE mode was first pointed out [to] efforts that needed to bind to a ciphertext some cleartext data, such as an IP address."
- **IETF draft AAD binding** (https://khimananda.com/blog/authenticated-encryption-aead-explained/): "The tag covers this metadata ensuring integrity and binding it to the ciphertext without revealing its contents."

**Findings:**
- The spec states (§7.1) that Header0 carries ordered_sources in cleartext and every layer's AAD binds the whole of Header0.
- This means: **each layer_i's AEAD tag is computed over (key_i, nonce_i, AAD={Header0, ...}, ciphertext_i).**
- **Attack scenario:** If an adversary reorders the ordered_sources array in Header0, the AAD changes, invalidating all AEAD tags. ✓ **Binding is tight.**
- **Additional check needed:** Verify that layer_index or layer_id is also included in the AAD (or nonce derivation) to prevent binding a decrypted layer to the wrong source entry by index lookup alone.

**Verdict:** IMPORTANT — **Verify that layer_index or layer_id participates in either AAD or nonce derivation.** If only ordered_sources ordering is enforced and layer_index is loose, an off-by-one or rotation attack may be possible. Assuming layer_index is deterministic (layer_i ← ordered_sources[i]), the binding is sound; **confirm this in spec.**

---

### 3. Capsule_salt Lifecycle — Generation, Uniqueness, Reuse Across Re-Wraps

**Issue:** What are the uniqueness and reuse guarantees for capsule_salt across multiple wrapping layers and re-encryption operations?

**Authority:**
- **RFC 5869, Section 3.1** (https://datatracker.ietf.org/doc/html/rfc5869): "The pair (salt, info) should be unique for each derived subkey — if it is not, then identical input keying material (which could occur for reasons outside your control) will still yield different PRKs" (from StackExchange https://crypto.stackexchange.com/questions/101163/minimum-length-of-salt-and-info-for-hkdf).
- **StackExchange on HKDF salt reuse** (https://crypto.stackexchange.com/questions/97975/): "If no salt were used, and the value of HMAC-Hash(NULL, IKM) were somehow leaked, then all uses of HKDF later on using the same IKM and without salt would also be compromised."
- **Dhole Moments / HKDF understanding** (https://soatok.blog/2021/11/17/understanding-hkdf/): "For a PRNG that continuously produces outputs by applying HKDF to renewable pools of entropy, a salt value can be fixed and reused for multiple applications of HKDF. In key agreement protocols, salt is derived from authenticated public nonces."

**Findings:**
- **Per-layer salt:** If each layer uses the same capsule_salt but different info strings (layer_index, context), the (salt, info) pairs are unique → **safe.**
- **Across re-wraps:** If capsule_salt is reused with the same DEK on a re-wrap operation, and the info string is NOT incremented (e.g., no re-wrap version counter), the same PRK and derived keys are generated. If the nonce derivation is deterministic and depends only on capsule_salt and DEK, **nonce reuse becomes possible**, which is **catastrophic for AEAD (GCM or ChaCha20-Poly1305).**
- **Safe pattern:** capsule_salt is randomly generated once per envelope, reused across layers with unique (salt, info) per layer, and NEVER reused across re-wraps unless re-wrap version or timestamp is mixed into info or nonce derivation.

**Verdict:** CRITICAL — **If nonce derivation is deterministic and capsule_salt can be reused across re-wrap operations, verify that re-wrap version, timestamp, or random counter is mixed into the nonce or info string.** Reference: IETF draft on nonce-reuse catastrophe in GCM (https://crypto.stackexchange.com/questions/102472/) and re-encryption envelope patterns (https://github.com/pilinux/crypt/envelope).

---

## IMPORTANT FINDINGS

### 4. Body_digest — Load-Bearing Purpose and Redundancy with AEAD Tag

**Issue:** What is body_digest actually load-bearing for, and is it redundant with the AEAD tag?

**Authority:**
- **RFC 9771 & Rogaway on AEAD** (https://datatracker.iacr.org/doc/rfc9771/): AEAD provides authenticated encryption: confidentiality (ciphertext is secret) and integrity (AEAD tag is keyed; only the holder of the key can forge a valid tag).
- **StackExchange on digest vs. AEAD tag** (https://security.stackexchange.com/questions/269129/aead-authenticating-a-digest-of-my-data-instead-the-data-itself): "Authenticating a digest instead of data itself is sound if the digest is part of AAD or if the digest is unkeyed and serves a different purpose (content addressing)."
- **Convergent encryption literature** (https://dl.acm.org/doi/fullHtml/10.1145/3365840): "Message-locked encryption (MLE) uses a hash of chunk content as a symmetric key; convergent encryption is one MLE instantiation."

**Findings:**
- **AEAD tag (keyed):** Proves to the key-holder that ciphertext has not been tampered with. Authenticates only if the verifier holds the decryption key.
- **Body digest (unkeyed, e.g., SHA-256):** Serves as a keyless, publicly verifiable content address. Two distinct purposes:
  1. **Content identity:** Allows deduplication, cross-system reference, and keyless integrity checking (e.g., backup verification without decryption).
  2. **Integrity in transit:** Independent of decryption key; useful in multi-stage supply chains where intermediate parties lack decryption keys.
- **NOT redundant:** The digest and tag serve different trust boundaries. A digest hashed over plaintext (pre-encryption) is NOT redundant with an AEAD tag over ciphertext.
- **Collision risk if digest over ciphertext:** If body_digest is computed over ciphertext instead of plaintext, convergent-encryption-style attacks become possible (low-entropy data can be brute-forced; see https://dl.acm.org/doi/fullHtml/10.1145/3365840).

**Verdict:** IMPORTANT — **Verify whether body_digest is computed over plaintext or ciphertext.** If plaintext: sound separation of concerns, no redundancy. If ciphertext: confirm plaintext entropy is high and document collision/brute-force assumptions. **Specify which hash algorithm and truncation length is used; minimum 256 bits (SHA-256) is recommended.**

---

### 5. Content Address — Layer Ownership of Semantics, Leak and Collision

**Issue:** Which layer owns the content-address semantics, and does addressing leak or enable confirmation-of-file attacks?

**Authority:**
- **Multihash spec** (https://github.com/multiformats/multihash & https://multiformats.io/multihash/): "Multihash is a self-describing hash format: `<varint-hash-function-code><varint-digest-length><digest-bytes>`. Format allows future-proofing against hash algorithm changes."
- **NIST SP 800-107 Revision 1** (https://nvlpubs.nist.gov/nistpubs/legacy/sp/nistspecialpublication800-107r1.pdf): "Truncating the message digest can impact the security of an application. For a b-bit hash (e.g., SHA-256 = 256 bits), collision resistance is ~2^(b/2) = 2^128 for SHA-256."
- **Convergent encryption attacks** (https://dl.acm.org/doi/pdf/10.1145/3342195.3387531): "Convergent encryption is vulnerable to brute-force attacks for data with low min-entropy if the content key (derived from content hash) is exposed or reused across multiple objects."

**Findings:**
- **Layer ownership:** If each layer has its own content address, which layer is canonical? Spec must clarify: is the top-level envelope's content address over the final ciphertext or plaintext?
- **Leakage:** Content address over ciphertext reveals which files are identical (deduplication oracle). This is NOT a leak of file content, but of file equality.
- **Confirmation-of-file attacks:** If an attacker can upload ciphertext and observe whether the server responds with "file exists," they can confirm file identity without decryption keys (known plaintext confirmation).
- **Collision bounds:** For a 256-bit digest (SHA-256), collision resistance is ~2^128 operations (birthday bound). If digest is truncated to 128 bits, collision cost drops to ~2^64. Verify that the spec's digest length matches intended collision-resistance level.

**Verdict:** IMPORTANT — **Specify which layer owns the canonical content address (envelope top-level or per-layer), digest algorithm (SHA-256 / SHA-512 / BLAKE2), and full bit-length (no truncation below 256 bits for SHA-256). Document that content addressing leaks file-equality information, which is intentional for deduplication; confirm this is acceptable in the threat model.**

---

## MINOR FINDINGS

### 6. Tri-Colour DFS Boundary Vectors for Closure Verification — Cycles, Diamonds, Missing Ancestors

**Issue:** Does the closure-verification path correctly detect cycles, diamond dependencies, and missing ancestors using tri-colour DFS?

**Authority:**
- **StackExchange on DFS cycle detection** (https://stackoverflow.com/questions/19113189/detecting-cycles-in-a-graph-using-dfs-2-different-approaches-and-what-is-the-difference/): "DFS reaches a vertex already in the current DFS path → cycle exists. Tri-colour tracking: unvisited (white), currently visiting (gray), completely visited (black)."
- **Medium on cycle detection in Java** (https://medium.com/@AlexanderObregon/detecting-cycles-in-graphs-with-depth-first-search-in-java-674ee583c2b7): "Cycle detection relies on tracking three possible states: unvisited, currently visiting, or completely visited."
- **Algorithms for Competitive Programming** (https://cp-algorithms.com/graph/finding-cycle.html): "To detect a cycle and reconstruct it: track parent pointers; on back edge detection (gray → gray), reconstruct cycle from cycle_end to cycle_start."

**Findings:**
- **Cycles:** Tri-colour DFS correctly detects cycles (back-edge to gray node). Verify that the spec runs DFS from all unvisited nodes to catch disconnected components.
- **Diamonds (join nodes):** A diamond (two paths merge at one node) is NOT a cycle. Tri-colour DFS handles this correctly: the join node is marked black on first completion, and its second incoming edge is recognized as a forward edge (to black node), not a back edge.
- **Missing ancestors:** If a ciphertext layer references a source not in ordered_sources, or references an index out of bounds, this is NOT a graph-theoretic issue; it is a **validation error** (must be caught by index-bounds checking before DFS runs).
- **Boundary vectors:** Spec should document what constitutes a valid closure: all referenced sources exist, graph is acyclic, all paths from sources to leaves are covered.

**Verdict:** MINOR — **Verify that closure-verification code runs DFS from all unvisited nodes (not just one root) to catch multi-component graphs. Confirm that index bounds are validated before DFS. No algorithmic flaw found in tri-colour DFS for cycle/diamond detection.**

---

## CROSS-CUTTING OBSERVATIONS

### Nonce Derivation Best Practices

Reference: GCM nonce reuse is catastrophic (https://crypto.stackexchange.com/questions/102472/, https://frereit.de/aes_gcm/). 

**Recommendation:** If nonce is derived deterministically from DEK + capsule_salt + layer_index, **verify that re-wrapping generates a fresh capsule_salt or includes a per-wrap counter/timestamp in the nonce derivation.** Example pattern:
- **Per-layer (within one envelope):** `nonce_i ← HKDF-Expand(PRK, "nonce" || layer_i || ordered_sources[i].capsule_id, 12)`  — **Safe** (info includes layer_i).
- **Across re-wraps:** If capsule_salt is reused from prior wrap, re-wrap counter MUST be included: `nonce ← HKDF-Expand(PRK, "nonce" || layer_i || wrap_version || timestamp, 12)`.

---

### DEK Semantics Across Layers

**Authority:** Best practices in envelope encryption (https://www.encryptionconsulting.com/envelope-encryption-kek-vs-dek-and-key-wrapping/): "A DEK should be generated fresh per encryption operation; reusing a DEK across multiple objects means compromising one DEK compromises all of them."

**Recommendation:** Clarify in spec whether each layer has its own DEK or if all layers share one DEK (derived via HKDF from a root key). If layers share one DEK:
- **Acceptable if:** HKDF derives per-layer DEKs from a root key with unique (salt, info) pairs per layer, or each layer's DEK is independently random.
- **Risky if:** One DEK is used directly to encrypt multiple layers without HKDF separation.

---

## Summary Table

| Focus Point | Severity | Finding | Required Action |
|---|---|---|---|
| 1. HKDF(DEK, ...) | IMPORTANT | DEK randomness assumption not stated | Explicit spec clause: "DEK ∼ Uniform(256 bits)" or document HKDF-Extract step |
| 2. ordered_sources ↔ layer_index | IMPORTANT | Binding is tight if layer_index in AAD/nonce | Verify layer_index participates in nonce or AAD derivation |
| 3. capsule_salt reuse | CRITICAL | Risk of nonce reuse on re-wraps if salt reused without version | Require (salt, info, wrap_version) uniqueness or fresh salt per wrap |
| 4. body_digest redundancy | IMPORTANT | Digest NOT redundant; serves keyless verification | Specify: plaintext/ciphertext, hash algorithm, bit-length (≥256) |
| 5. content_address semantics | IMPORTANT | Layer ownership and collision bounds unclear | Specify: canonical layer, hash algorithm, truncation, intentional equality-leakage |
| 6. tri-colour DFS closure | MINOR | Algorithm correct for cycles/diamonds | Verify: DFS covers all components, index bounds checked before traversal |

---

## Final Verdict

**The envelope parameter set is SOUND with three clarifications required:**

1. **Require explicit DEK randomness assumption** (state "DEK ∼ Uniform(≥256 bits)" or include HKDF-Extract step).
2. **Verify layer_index or layer_id participates in nonce derivation or AAD binding** to fully pin ordered_sources ↔ ciphertext correspondence.
3. **Document nonce derivation across re-wraps** to guarantee that (salt, info) or (salt, info, wrap_version) is unique and never repeated with the same DEK/PRK.

All other parameters (body_digest, content addressing, closure verification) are sound once the above clarifications are added to the spec.

---

## Research Sources (Compact Digest)

| Topic | Source | Clause / Authority |
|---|---|---|
| AEAD properties | RFC 9771 | §2: AEAD = (key, nonce, AAD, plaintext) → (ciphertext, tag) |
| HKDF uniqueness | RFC 5869 | §3.1: (salt, info) must be unique per derived key; IKM assumed high-entropy or extracted |
| Nonce reuse risk | Cryptography.io & StackExchange | GCM/ChaCha20-Poly1305 nonce reuse = catastrophic (tag/plaintext leakage) |
| Convergent encryption | ACM Transactions (https://dl.acm.org/doi/fullHtml/10.1145/3365840) | Low-entropy data vulnerable to brute-force if content-key derivation exposed |
| AAD binding purpose | Rogaway (https://web.cs.ucdavis.edu/~rogaway/papers/ad.pdf) | AAD binds cleartext metadata to ciphertext without encrypting metadata |
| DFS cycle detection | Medium & Algo CP (https://cp-algorithms.com/graph/) | Tri-colour (white/gray/black) correctly detects cycles and diamonds |
| Content addressing | Multihash / IPFS | Self-describing format with hash-function and length codes; prevents hash-agility failures |
| Hash truncation | NIST SP 800-107 | Truncation impacts collision resistance; for SHA-256, full 256 bits → 2^128 collision cost |

---

## MUR 裁決（2026-09-22，本檔落地後補記）

評審者受「僅網路研究、不得讀本地檔案」約束，故部分發現是由第一原理推得，而非對照契約正文。逐點核對後的處置如下。

| 發現 | 處置 | 依據 |
|---|---|---|
| CRITICAL — epoch rewrap 下 HKDF `(salt, info)` 重用 | **採納並已修訂** | §3.1 要求 epoch advance 重包所有存活 slot guard，但 `capsule_salt`／`Header0`／`slot_guard` 都不重新產生，兩次 rewrap 會導出相同子金鑰。§4.1 兩條 HKDF info 已各自加入 `epoch`；解鎖端本就從 manifest 取得，不需新增儲存或改動 wire 結構 |
| #2、#4 | **不列為缺口** | 契約正文已有答案，評審者因不得讀本地檔而重推。非評審者之誤，屬送審條件所致 |
| #1、#3、#5、#6 | **待裁決** | 尚未處置 |

送審方法的修正：後續密碼學評審應附契約唯讀快照，或明列「契約已答、不需重答」的條目，否則會重複出現「評審花時間猜、我方花時間駁」的循環。
