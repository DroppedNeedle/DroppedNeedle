<script lang="ts">
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { AUTH_ENDPOINTS } from '$lib/queries/auth/endpoints';
	import { createSettingsForm } from '$lib/utils/settingsForm.svelte';
	import { onDestroy } from 'svelte';
	import {
		ShieldAlert,
		ShieldCheck,
		KeyRound,
		CircleCheck,
		CircleAlert,
		Save,
		RotateCcw,
		FolderSearch,
		LogIn,
		Download
	} from 'lucide-svelte';
	import { api } from '$lib/api/client';
	import type { OIDCConnectionSettings } from '$lib/types';
	import type { components } from '$lib/api/v3/openapi';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import ListField from '$lib/ui/ListField.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import SettingsPublicAddress from './SettingsPublicAddress.svelte';

	type SecuritySettingsForm = components['schemas']['SecuritySettings'];
	type PolicyMode = SecuritySettingsForm['content_security_policy'];

	const DEFAULTS: SecuritySettingsForm = {
		hibp_check: true,
		hibp_local_path: '',
		hsts_max_age: 0,
		hsts_include_subdomains: false,
		hsts_preload: false,
		library_download_access: 'everyone',
		content_security_policy: 'report_only',
		frame_ancestors: []
	};

	const MAX_FRAMING_ADDRESSES = 8;
	const OIDC_SECRET_MASK = 'oidc****';

	const POLICY_MODES: { value: PolicyMode; label: string }[] = [
		{ value: 'enforce', label: 'Enforce' },
		{ value: 'report_only', label: 'Report only' },
		{ value: 'off', label: 'Off' }
	];

	const POLICY_MODE_HELP: Record<PolicyMode, string> = {
		enforce: 'The browser refuses anything the policy does not allow.',
		report_only:
			'The browser lists in its console what the policy would refuse and refuses it only for frames: who may show DroppedNeedle in a frame, and what its frames may open.',
		off: 'No policy is sent, so the browser limits nothing and anyone may show DroppedNeedle in a frame.'
	};

	const form = createSettingsForm<SecuritySettingsForm>({
		loadEndpoint: SETTINGS_ENDPOINTS.security(),
		saveEndpoint: SETTINGS_ENDPOINTS.security(),
		defaultValue: DEFAULTS
	});

	let verifying = $state(false);
	let verifyResult = $state<{ valid: boolean; message: string } | null>(null);

	type OIDCTestResult = { valid: boolean; message: string };
	type OIDCSettingsForm = ReturnType<typeof createSettingsForm<OIDCConnectionSettings>> & {
		testResult: OIDCTestResult | null;
	};

	const oidcForm = createSettingsForm<OIDCConnectionSettings>({
		loadEndpoint: SETTINGS_ENDPOINTS.oidc(),
		saveEndpoint: SETTINGS_ENDPOINTS.oidc(),
		testEndpoint: SETTINGS_ENDPOINTS.oidcVerify(),
		enabledField: 'enabled'
	}) as OIDCSettingsForm;

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
	const oidcToggleHelp = $derived(
		!hasOidcCredentials
			? 'Enter a client ID and redirect URI to enable SSO.'
			: !oidcForm.testResult?.valid && !oidcForm.wasAlreadyEnabled
				? 'Test and get a valid connection to enable'
				: 'Let users sign in to DroppedNeedle with this SSO provider'
	);

	const DOWNLOAD_ACCESS = [
		{
			value: 'everyone',
			label: 'Everyone',
			help: 'Any logged-in user can download. This is the default.'
		},
		{
			value: 'trusted',
			label: 'Trusted users and admins',
			help: 'Regular users can stream but not download.'
		},
		{ value: 'admin', label: 'Admins only', help: 'Only admins can download.' }
	] as const;

	async function verifyLocalFile() {
		if (!form.data) return;
		verifying = true;
		verifyResult = null;
		try {
			verifyResult = await api.v3.POST(SETTINGS_ENDPOINTS.securityVerifyHibp(), form.data);
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
			Object.assign(form.data, DEFAULTS, { frame_ancestors: [] });
		}
	}

	// A row left blank is not an address: drop it before the server sees it.
	async function saveSecurity() {
		if (!form.data) return;
		form.data.frame_ancestors = form.data.frame_ancestors
			.map((address) => address.trim())
			.filter(Boolean);
		await form.save();
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
		<Card>
			{#snippet header()}
				<div class="flex items-center gap-2">
					<div class="bg-primary/10 rounded-lg p-2">
						<KeyRound class="h-5 w-5 text-primary" />
					</div>
					<div>
						<h3 class="font-semibold">Password Security</h3>
						<p class="text-xs text-base-content/50">Controls applied when accounts are created.</p>
					</div>
				</div>
			{/snippet}
			<div class="mt-2 flex flex-col gap-4">
				<ToggleRow
					label="Check passwords against Have I Been Pwned"
					description="Rejects passwords found in known data breaches. Uses k-anonymity, only the first 5 characters of the password's SHA-1 hash are sent. The full password is never transmitted."
					bind:checked={form.data.hibp_check}
				/>

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

					<div>
						<Field label="Local database path (optional)">
							{#snippet children(control)}
								<div class="flex gap-2">
									<Input
										{...control}
										label="Local database path (optional)"
										mono
										class="w-full"
										placeholder="/data/pwned-passwords-sha1-ordered-by-hash-v8.txt"
										bind:value={form.data!.hibp_local_path}
										oninput={() => (verifyResult = null)}
									/>
									<Button
										variant="ghost"
										icon={FolderSearch}
										class="shrink-0"
										loading={verifying}
										disabled={!form.data?.hibp_local_path.trim()}
										onclick={() => void verifyLocalFile()}
									>
										Verify
									</Button>
								</div>
							{/snippet}
						</Field>
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
		</Card>

		<!-- Library downloads -->
		<Card>
			{#snippet header()}
				<div class="flex items-center gap-2">
					<div class="bg-success/10 rounded-lg p-2">
						<Download class="h-5 w-5 text-success" />
					</div>
					<div>
						<h3 class="font-semibold">Library downloads</h3>
						<p class="text-xs text-base-content/50">
							Who can download library files and full-album archives. Streaming is never restricted.
						</p>
					</div>
				</div>
			{/snippet}
			<fieldset class="mt-2 flex flex-wrap gap-x-8 gap-y-3">
				<legend class="sr-only">Who can download</legend>
				{#each DOWNLOAD_ACCESS as option (option.value)}
					<label class="flex cursor-pointer items-start gap-4">
						<input
							type="radio"
							name="library-download-access"
							class="radio radio-sm mt-0.5"
							value={option.value}
							bind:group={form.data.library_download_access}
						/>
						<span>
							<span class="block text-sm font-medium">{option.label}</span>
							<span class="mt-0.5 block text-xs text-base-content/50">{option.help}</span>
						</span>
					</label>
				{/each}
			</fieldset>
		</Card>

		<!-- HSTS -->
		<Card>
			{#snippet header()}
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
			{/snippet}
			<div class="mt-2 flex flex-col gap-4">
				<div class="alert alert-warning alert-soft text-sm">
					<CircleAlert class="h-4 w-4 shrink-0" />
					<span>
						<strong>Only enable if nothing on this hostname is reached over plain HTTP.</strong> A browser
						that has seen the header refuses plain HTTP to this hostname, on every port, for as long as
						Max-Age says. That covers any other service on the same hostname too. The header is only sent
						on requests that arrive over HTTPS.
					</span>
				</div>

				<div>
					<Field label="Max-Age (seconds)" help="0 = disabled, 63072000 (two years) at most">
						{#snippet children(control)}
							<div class="flex gap-2 items-center flex-wrap">
								<Input
									{...control}
									label="Max-Age (seconds)"
									type="number"
									min="0"
									max="63072000"
									class="w-40"
									bind:value={form.data!.hsts_max_age}
								/>
								<div class="flex gap-1 flex-wrap">
									{#each HSTS_PRESETS as preset (preset.value)}
										<Button
											size="xs"
											variant={form.data?.hsts_max_age === preset.value ? 'primary' : 'ghost'}
											aria-pressed={form.data?.hsts_max_age === preset.value}
											onclick={() => {
												if (form.data) form.data.hsts_max_age = preset.value;
											}}
										>
											{preset.label}
										</Button>
									{/each}
								</div>
							</div>
						{/snippet}
					</Field>
					<p class="text-xs text-base-content/50 mt-1 ml-1">
						Recommended starting value when behind HTTPS: <strong>1 month</strong>. Increase
						gradually after confirming everything works.
					</p>
				</div>

				<div class="divider my-0"></div>

				<ToggleRow
					label="Include Subdomains"
					description="Applies HSTS to all subdomains of your domain. Only enable if all subdomains also serve HTTPS."
					bind:checked={form.data.hsts_include_subdomains}
					disabled={form.data.hsts_max_age === 0}
				/>

				<div>
					<ToggleRow
						label="Preload"
						description="Adds the preload directive. Very difficult to undo."
						bind:checked={form.data.hsts_preload}
						disabled={form.data.hsts_max_age === 0}
					/>
					<p class="mt-0.5 text-sm text-base-content/60">
						Only enable after registering your domain at
						<a
							href="https://hstspreload.org"
							target="_blank"
							rel="noopener noreferrer"
							class="link link-accent">hstspreload.org</a
						>.
					</p>
				</div>

				{#if form.data.hsts_preload && !form.data.hsts_include_subdomains}
					<div class="alert alert-error alert-soft text-sm">
						<CircleAlert class="h-4 w-4 shrink-0" />
						<span>Preload requires Include Subdomains to also be enabled.</span>
					</div>
				{/if}
			</div>
		</Card>

		<!-- Content-security policy -->
		<Card>
			{#snippet header()}
				<div class="flex items-center gap-2">
					<div class="bg-warning/10 rounded-lg p-2">
						<ShieldAlert class="h-5 w-5 text-warning" />
					</div>
					<div>
						<h3 class="font-semibold">Content-security policy</h3>
						<p class="text-xs text-base-content/50">
							Tells the browser what the web UI may load and run, and who may show it in a frame.
						</p>
					</div>
				</div>
			{/snippet}
			<div class="mt-2 flex flex-col gap-4">
				<SelectField
					label="Policy"
					bind:value={form.data.content_security_policy}
					options={POLICY_MODES}
					help={POLICY_MODE_HELP[form.data.content_security_policy]}
				/>

				<p class="text-sm text-base-content/70">
					A reverse proxy that rewrites the page can leave the web UI blank while the policy is
					enforced. If that happens, stop the server and run
					<code class="text-xs">droppedneedle-tool content-security-policy report-only</code> on the host.
				</p>

				<ListField
					label="Allowed to frame DroppedNeedle"
					itemLabel="Address"
					addLabel="Add an address"
					bind:values={form.data.frame_ancestors}
					max={MAX_FRAMING_ADDRESSES}
					mono
					placeholder="https://home.example.com"
					empty="No address listed: nobody may show DroppedNeedle in a frame."
					help="Each address listed here, such as a dashboard at https://home.example.com, may show DroppedNeedle in a frame unless the policy is off. A dashboard on the same site (another subdomain of the same domain, or the same host on another port) works; one on another site shows the sign-in page, because the browser does not send the sign-in cookie there. The scheme, host and port, no path. Up to {MAX_FRAMING_ADDRESSES}."
					disabled={form.saving}
				/>
			</div>
		</Card>

		<SettingsPublicAddress />

		<!-- OIDC / SSO -->
		<Card>
			{#snippet header()}
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
			{/snippet}
			<div class="mt-2 flex flex-col gap-4">
				{#if oidcForm.loading}
					<div class="flex justify-center items-center py-12">
						<span class="loading loading-spinner loading-lg"></span>
					</div>
				{:else if oidcForm.data}
					<TextField
						label="Issuer URL"
						type="url"
						placeholder="https://accounts.example.com"
						help="The base URL of your OIDC provider. Must serve a /.well-known/openid-configuration document."
						bind:value={oidcForm.data.issuer}
					/>

					<TextField label="Client ID" bind:value={oidcForm.data.client_id} />

					<SecretField
						label="Client Secret"
						mask={OIDC_SECRET_MASK}
						help="Optional - leave blank for public/PKCE clients"
						bind:value={oidcForm.data.client_secret}
					/>

					<TextField
						label="Redirect URI"
						type="url"
						placeholder={`https://droppedneedle.example.com${AUTH_ENDPOINTS.oidcCallback}`}
						help={`This must point to the DroppedNeedle backend API (it ends in ${AUTH_ENDPOINTS.oidcCallback}), not the web app page. Register this exact URL as a redirect/callback URI with your provider.`}
						bind:value={oidcForm.data.redirect_uri}
					/>

					<TextField
						label="Scopes"
						placeholder="openid email profile"
						bind:value={oidcForm.data.scopes}
					/>

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

					<ToggleRow
						label="Allow login with SSO"
						description={oidcToggleHelp}
						bind:checked={oidcForm.data.enabled}
						disabled={oidcToggleDisabled}
					/>

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
						<Button
							variant="ghost"
							loading={oidcForm.testing}
							disabled={!oidcForm.data.issuer}
							onclick={testOidc}
						>
							Test Connection
						</Button>
						<Button variant="primary" loading={oidcForm.saving} onclick={saveOidc}>
							Save Settings
						</Button>
					</div>
				{/if}
			</div>
		</Card>

		<div class="flex justify-end gap-3 pt-2">
			<Button variant="ghost" icon={RotateCcw} onclick={resetToDefaults} disabled={form.saving}>
				Reset
			</Button>
			<AsyncButton
				variant="primary"
				icon={Save}
				onclick={saveSecurity}
				pending={form.saving}
				pendingLabel="Saving…"
				disabled={form.data.hsts_preload && !form.data.hsts_include_subdomains}
			>
				Save Settings
			</AsyncButton>
		</div>
	{/if}
</div>
