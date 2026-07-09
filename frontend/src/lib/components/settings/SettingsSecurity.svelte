<script lang="ts">
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import {
		ShieldCheck,
		KeyRound,
		CircleCheck,
		CircleAlert,
		Save,
		RotateCcw,
		FolderSearch,
		LogIn,
		Eye,
		EyeOff
	} from 'lucide-svelte';
	import { api } from '$lib/api/client';
	import type { OIDCConnectionSettings } from '$lib/types';

	interface SecuritySettingsForm {
		hibp_check: boolean;
		hibp_local_path: string;
		hsts_max_age: number;
		hsts_include_subdomains: boolean;
		hsts_preload: boolean;
	}

	const DEFAULTS: SecuritySettingsForm = {
		hibp_check: true,
		hibp_local_path: '',
		hsts_max_age: 0,
		hsts_include_subdomains: false,
		hsts_preload: false
	};

	const form = createSettingsForm<SecuritySettingsForm>({
		loadEndpoint: '/api/v1/settings/security',
		saveEndpoint: '/api/v1/settings/security',
		defaultValue: DEFAULTS
	});

	let verifying = $state(false);
	let verifyResult = $state<{ valid: boolean; message: string } | null>(null);

	type OIDCTestResult = { valid: boolean; message: string };
	type OIDCSettingsForm = ReturnType<typeof createSettingsForm<OIDCConnectionSettings>> & {
		testResult: OIDCTestResult | null;
	};

	const oidcForm = createSettingsForm<OIDCConnectionSettings>({
		loadEndpoint: '/api/v1/settings/oidc',
		saveEndpoint: '/api/v1/settings/oidc',
		testEndpoint: '/api/v1/settings/oidc/verify',
		enabledField: 'enabled'
	}) as OIDCSettingsForm;

	let showClientSecret = $state(false);

	async function testOidc() {
		await oidcForm.test();
	}

	async function saveOidc() {
		await oidcForm.save();
	}

	// Only ID + redirect URI gate the toggle: PKCE lets public clients log in
	// without a client secret, so the secret stays optional.
	const hasOidcCredentials = $derived(
		Boolean(oidcForm.data?.client_id) && Boolean(oidcForm.data?.redirect_uri)
	);
	const oidcToggleDisabled = $derived(
		!hasOidcCredentials || (!oidcForm.testResult?.valid && !oidcForm.wasAlreadyEnabled)
	);

	async function verifyLocalFile() {
		if (!form.data) return;
		verifying = true;
		verifyResult = null;
		try {
			verifyResult = await api.post<{ valid: boolean; message: string }>(
				'/api/v1/settings/security/verify-hibp',
				form.data
			);
		} catch (e: unknown) {
			verifyResult = {
				valid: false,
				message: (e as { message?: string })?.message ?? 'Verification failed'
			};
		} finally {
			verifying = false;
		}
	}

	$effect(() => {
		form.load();
		oidcForm.load();
	});
	onDestroy(() => {
		form.cleanup();
		oidcForm.cleanup();
	});

	const HSTS_PRESETS = [
		{ label: 'Disabled', value: 0 },
		{ label: '1 month', value: 30 * 24 * 3600 },
		{ label: '6 months', value: 180 * 24 * 3600 },
		{ label: '1 year', value: 365 * 24 * 3600 }
	];

	function resetToDefaults() {
		if (form.data) {
			Object.assign(form.data, DEFAULTS);
		}
	}
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-2xl font-bold">Security</h2>
		<p class="text-base-content/60 mt-1">
			Configure password security and HTTPS enforcement headers.
		</p>
	</div>

	{#if form.loading}
		<div class="flex justify-center items-center py-20">
			<span class="loading loading-spinner loading-lg text-primary"></span>
		</div>
	{:else if form.data}
		{#if form.message}
			<div
				class="alert {form.messageType === 'success' ? 'alert-success' : 'alert-error'} alert-soft"
			>
				{#if form.messageType === 'success'}
					<CircleCheck class="w-5 h-5 shrink-0" />
				{:else}
					<CircleAlert class="w-5 h-5 shrink-0" />
				{/if}
				<span>{form.message}</span>
			</div>
		{/if}

		<!-- Password Security -->
		<div class="card bg-base-200">
			<div class="card-body gap-4">
				<div class="flex items-center gap-2">
					<div class="bg-primary/10 rounded-lg p-2">
						<KeyRound class="h-5 w-5 text-primary" />
					</div>
					<div>
						<h3 class="font-semibold">Password Security</h3>
						<p class="text-xs text-base-content/50">Controls applied when accounts are created.</p>
					</div>
				</div>

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							class="toggle toggle-primary"
							bind:checked={form.data.hibp_check}
						/>
						<div>
							<span class="label-text font-medium">Check passwords against Have I Been Pwned</span>
							<p class="text-xs text-base-content/50 mt-0.5">
								Rejects passwords found in known data breaches. Uses k-anonymity, only the first 5
								characters of the password's SHA-1 hash are sent. The full password is never
								transmitted.
							</p>
						</div>
					</label>
				</div>

				{#if !form.data.hibp_check}
					<div class="alert alert-warning alert-soft text-sm">
						<CircleAlert class="h-4 w-4 shrink-0" />
						<span
							>HIBP checking is disabled. Users may set passwords from known breach databases. Only
							turn this off for air-gapped or offline installs.</span
						>
					</div>
				{/if}

				{#if form.data.hibp_check}
					<div class="divider my-1"></div>

					<div class="form-control w-full">
						<label class="label" for="hibp-local-path">
							<span class="label-text font-medium"
								>Local database path <span class="text-base-content/40 font-normal">(optional)</span
								></span
							>
						</label>
						<div class="flex gap-2">
							<input
								id="hibp-local-path"
								type="text"
								class="input w-full font-mono text-sm"
								placeholder="/data/pwned-passwords-sha1-ordered-by-hash-v8.txt"
								bind:value={form.data.hibp_local_path}
								oninput={() => (verifyResult = null)}
							/>
							<button
								type="button"
								class="btn btn-ghost gap-1.5 shrink-0"
								onclick={() => void verifyLocalFile()}
								disabled={verifying || !form.data.hibp_local_path.trim()}
							>
								{#if verifying}
									<span class="loading loading-spinner loading-xs"></span>
								{:else}
									<FolderSearch class="h-4 w-4" />
								{/if}
								Verify
							</button>
						</div>
						<p class="text-xs text-base-content/50 mt-1.5 ml-1 space-y-1">
							Absolute path to a local copy of the HIBP
							<a
								href="https://haveibeenpwned.com/Passwords"
								target="_blank"
								rel="noopener noreferrer"
								class="link link-primary">"Pwned Passwords" file (ordered by hash)</a
							>. When set and the file exists, no outbound API calls are made. If the path is set
							but the file is missing, the check is skipped rather than falling back to the API.
						</p>
					</div>

					{#if verifyResult}
						<div
							class="alert {verifyResult.valid
								? 'alert-success'
								: 'alert-error'} alert-soft text-sm"
						>
							{#if verifyResult.valid}
								<CircleCheck class="h-4 w-4 shrink-0" />
							{:else}
								<CircleAlert class="h-4 w-4 shrink-0" />
							{/if}
							<span>{verifyResult.message}</span>
						</div>
					{/if}

					{#if !form.data.hibp_local_path.trim()}
						<p class="text-xs text-base-content/40 ml-1">
							No local path set, will use the HIBP k-anonymity API for each new account.
						</p>
					{/if}
				{/if}
			</div>
		</div>

		<!-- HSTS -->
		<div class="card bg-base-200">
			<div class="card-body gap-4">
				<div class="flex items-center gap-2">
					<div class="bg-accent/10 rounded-lg p-2">
						<ShieldCheck class="h-5 w-5 text-accent" />
					</div>
					<div>
						<h3 class="font-semibold">HTTP Strict Transport Security (HSTS)</h3>
						<p class="text-xs text-base-content/50">
							Tells browsers to only connect over HTTPS. Leave disabled for local HTTP installs.
						</p>
					</div>
				</div>

				<div class="alert alert-warning alert-soft text-sm">
					<CircleAlert class="h-4 w-4 shrink-0" />
					<span>
						<strong>Only enable if you are serving DroppedNeedle over HTTPS.</strong> Enabling HSTS on
						a plain HTTP install will lock users out until the HSTS header expires in their browser.
					</span>
				</div>

				<div class="form-control w-full">
					<label class="label" for="hsts-max-age">
						<span class="label-text font-medium">Max-Age (seconds)</span>
						<span class="label-text-alt text-base-content/40">0 = disabled</span>
					</label>
					<div class="flex gap-2 items-center flex-wrap">
						<input
							id="hsts-max-age"
							type="number"
							min="0"
							class="input w-40"
							bind:value={form.data.hsts_max_age}
						/>
						<div class="flex gap-1 flex-wrap">
							{#each HSTS_PRESETS as preset (preset.value)}
								<button
									type="button"
									class="btn btn-xs {form.data.hsts_max_age === preset.value
										? 'btn-primary'
										: 'btn-ghost'}"
									onclick={() => {
										if (form.data) form.data.hsts_max_age = preset.value;
									}}
								>
									{preset.label}
								</button>
							{/each}
						</div>
					</div>
					<p class="text-xs text-base-content/50 mt-1 ml-1">
						Recommended starting value when behind HTTPS: <strong>1 month</strong>. Increase
						gradually after confirming everything works.
					</p>
				</div>

				<div class="divider my-0"></div>

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							class="toggle toggle-accent toggle-sm"
							bind:checked={form.data.hsts_include_subdomains}
							disabled={form.data.hsts_max_age === 0}
						/>
						<div>
							<span class="label-text font-medium">Include Subdomains</span>
							<p class="text-xs text-base-content/50 mt-0.5">
								Applies HSTS to all subdomains of your domain. Only enable if <em>all</em> subdomains
								also serve HTTPS.
							</p>
						</div>
					</label>
				</div>

				<div class="form-control">
					<label class="label cursor-pointer justify-start gap-4">
						<input
							type="checkbox"
							class="toggle toggle-accent toggle-sm"
							bind:checked={form.data.hsts_preload}
							disabled={form.data.hsts_max_age === 0}
						/>
						<div>
							<span class="label-text font-medium">Preload</span>
							<p class="text-xs text-base-content/50 mt-0.5">
								Adds the <code class="text-xs">preload</code> directive. Only enable after
								registering your domain at
								<a
									href="https://hstspreload.org"
									target="_blank"
									rel="noopener noreferrer"
									class="link link-accent">hstspreload.org</a
								>. This is very difficult to undo.
							</p>
						</div>
					</label>
				</div>

				{#if form.data.hsts_preload && !form.data.hsts_include_subdomains}
					<div class="alert alert-error alert-soft text-sm">
						<CircleAlert class="h-4 w-4 shrink-0" />
						<span>Preload requires Include Subdomains to also be enabled.</span>
					</div>
				{/if}
			</div>
		</div>

		<!-- OIDC / SSO -->
		<div class="card bg-base-200">
			<div class="card-body gap-4">
				<div class="flex items-center gap-2">
					<div class="bg-info/10 rounded-lg p-2">
						<LogIn class="h-5 w-5 text-info" />
					</div>
					<div>
						<h3 class="font-semibold">Single Sign-On (OIDC)</h3>
						<p class="text-xs text-base-content/50">
							Let users sign in with an external OpenID Connect provider.
						</p>
					</div>
				</div>

				{#if oidcForm.loading}
					<div class="flex justify-center items-center py-12">
						<span class="loading loading-spinner loading-lg"></span>
					</div>
				{:else if oidcForm.data}
					<div class="form-control w-full">
						<label class="label" for="oidc-issuer">
							<span class="label-text font-medium">Issuer URL</span>
						</label>
						<input
							id="oidc-issuer"
							type="url"
							bind:value={oidcForm.data.issuer}
							class="input input-bordered w-full"
							placeholder="https://accounts.example.com"
						/>
						<p class="text-xs text-base-content/50 mt-1.5 ml-1">
							The base URL of your OIDC provider. Must serve a
							<code class="text-xs">/.well-known/openid-configuration</code> document.
						</p>
					</div>

					<div class="form-control w-full">
						<label class="label" for="oidc-client-id">
							<span class="label-text font-medium">Client ID</span>
						</label>
						<input
							id="oidc-client-id"
							type="text"
							bind:value={oidcForm.data.client_id}
							class="input input-bordered w-full"
						/>
					</div>

					<div class="form-control w-full">
						<label class="label" for="oidc-client-secret">
							<span class="label-text font-medium">Client Secret</span>
							<span class="label-text-alt opacity-60"
								>optional - leave blank for public/PKCE clients</span
							>
						</label>
						<label class="input input-bordered flex items-center gap-2 w-full">
							{#if showClientSecret}
								<input
									id="oidc-client-secret"
									type="text"
									class="grow"
									bind:value={oidcForm.data.client_secret}
								/>
							{:else}
								<input
									id="oidc-client-secret"
									type="password"
									class="grow"
									bind:value={oidcForm.data.client_secret}
								/>
							{/if}
							<button
								type="button"
								onclick={() => (showClientSecret = !showClientSecret)}
								class="opacity-50 hover:opacity-100 transition-opacity"
								aria-label="Toggle client secret visibility"
							>
								{#if showClientSecret}<EyeOff class="h-4 w-4" />{:else}<Eye class="h-4 w-4" />{/if}
							</button>
						</label>
					</div>

					<div class="form-control w-full">
						<label class="label" for="oidc-redirect-uri">
							<span class="label-text font-medium">Redirect URI</span>
						</label>
						<input
							id="oidc-redirect-uri"
							type="url"
							bind:value={oidcForm.data.redirect_uri}
							class="input input-bordered w-full"
							placeholder="https://droppedneedle.example.com/api/v1/auth/oidc/callback"
						/>
						<p class="text-xs text-base-content/50 mt-1.5 ml-1">
							This must point to the DroppedNeedle <strong>backend API</strong> (it ends in
							<code class="text-xs">/api/v1/auth/oidc/callback</code>), not the web app page.
							Register this exact URL as a redirect/callback URI with your provider.
						</p>
					</div>

					<div class="form-control w-full">
						<label class="label" for="oidc-scopes">
							<span class="label-text font-medium">Scopes</span>
						</label>
						<input
							id="oidc-scopes"
							type="text"
							bind:value={oidcForm.data.scopes}
							class="input input-bordered w-full"
							placeholder="openid email profile"
						/>
					</div>

					{#if oidcForm.testResult}
						<div
							class="alert {oidcForm.testResult.valid
								? 'alert-success'
								: 'alert-error'} alert-soft text-sm"
						>
							{#if oidcForm.testResult.valid}
								<CircleCheck class="h-4 w-4 shrink-0" />
							{:else}
								<CircleAlert class="h-4 w-4 shrink-0" />
							{/if}
							<span>{oidcForm.testResult.message}</span>
						</div>
					{/if}

					<div class="form-control">
						<label class="label cursor-pointer justify-start gap-4">
							<input
								type="checkbox"
								bind:checked={oidcForm.data.enabled}
								class="toggle toggle-primary"
								disabled={oidcToggleDisabled}
							/>
							<div>
								<span class="label-text font-medium">Allow login with SSO</span>
								<p class="text-xs text-base-content/50">
									{#if !hasOidcCredentials}
										Enter a client ID and redirect URI to enable SSO.
									{:else if !oidcForm.testResult?.valid && !oidcForm.wasAlreadyEnabled}
										Test and get a valid connection to enable
									{:else}
										Let users sign in to DroppedNeedle with this SSO provider
									{/if}
								</p>
							</div>
						</label>
					</div>

					{#if oidcForm.message}
						<div
							class="alert {oidcForm.messageType === 'success'
								? 'alert-success'
								: 'alert-error'} alert-soft text-sm"
						>
							<span>{oidcForm.message}</span>
						</div>
					{/if}

					<div class="flex justify-end gap-2 pt-2">
						<button
							type="button"
							class="btn btn-ghost"
							onclick={testOidc}
							disabled={oidcForm.testing || !oidcForm.data.issuer}
						>
							{#if oidcForm.testing}
								<span class="loading loading-spinner loading-sm"></span>
							{/if}
							Test Connection
						</button>
						<button
							type="button"
							class="btn btn-primary"
							onclick={saveOidc}
							disabled={oidcForm.saving}
						>
							{#if oidcForm.saving}
								<span class="loading loading-spinner loading-sm"></span>
							{/if}
							Save Settings
						</button>
					</div>
				{/if}
			</div>
		</div>

		<div class="flex justify-end gap-3 pt-2">
			<button class="btn btn-ghost" onclick={resetToDefaults} disabled={form.saving}>
				<RotateCcw class="w-4 h-4" />
				Reset
			</button>
			<button
				class="btn btn-primary"
				onclick={() => form.save()}
				disabled={form.saving || (form.data.hsts_preload && !form.data.hsts_include_subdomains)}
			>
				{#if form.saving}
					<span class="loading loading-spinner loading-sm"></span>
					Saving…
				{:else}
					<Save class="w-4 h-4" />
					Save Settings
				{/if}
			</button>
		</div>
	{/if}
</div>
