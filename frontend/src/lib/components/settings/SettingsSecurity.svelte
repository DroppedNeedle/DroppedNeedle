<script lang="ts">
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { SETTINGS_ENDPOINTS } from '$lib/queries/settings/endpoints';
	import { AUTH_ENDPOINTS } from '$lib/queries/auth/endpoints';
	import {
		createSaveOidcSettingsMutation,
		createSaveSecuritySettingsMutation,
		createVerifyOidcMutation,
		getOidcSettingsQuery,
		getSecuritySettingsQuery,
		type SecuritySettings
	} from '$lib/queries/settings/SectionSettingsQueries.svelte';
	import {
		ShieldAlert,
		ShieldCheck,
		KeyRound,
		CircleCheck,
		CircleAlert,
		RotateCcw,
		FolderSearch,
		LogIn,
		Download
	} from 'lucide-svelte';
	import { api } from '$lib/api/client';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';
	import ListField from '$lib/ui/ListField.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import SelectField from '$lib/ui/SelectField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';
	import SettingsPublicAddress from './SettingsPublicAddress.svelte';

	type PolicyMode = SecuritySettings['content_security_policy'];

	const DEFAULTS: SecuritySettings = {
		hibp_check: true,
		hibp_local_path: '',
		hsts_max_age: 0,
		hsts_include_subdomains: false,
		hsts_preload: false,
		library_download_access: 'everyone',
		content_security_policy: 'enforce',
		frame_ancestors: []
	};

	const MAX_FRAMING_ADDRESSES = 8;

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

	// A row left blank is not an address: it is dropped before the server sees it.
	const security = createSettingsSection(
		getSecuritySettingsQuery(),
		createSaveSecuritySettingsMutation(),
		{
			normalize: (draft) => ({
				...draft,
				frame_ancestors: draft.frame_ancestors.map((address) => address.trim()).filter(Boolean)
			})
		}
	);

	let verifying = $state(false);
	let verifyResult = $state<{ valid: boolean; message: string } | null>(null);

	const verifyOidc = createVerifyOidcMutation();

	const oidc = createSettingsSection(getOidcSettingsQuery(), createSaveOidcSettingsMutation(), {
		test: async (draft) => {
			if (!draft.issuer) return { passed: false, message: 'Add the issuer URL first.' };
			const result = await verifyOidc.mutateAsync(draft);
			return { passed: result.valid, message: result.message };
		}
	});

	// Only ID + redirect URI gate the toggle: PKCE lets public clients log in
	// without a client secret, so the secret stays optional. The toggle stays
	// locked until a test passes, unless already on.
	const hasOidcCredentials = $derived(
		Boolean(oidc.draft?.client_id) && Boolean(oidc.draft?.redirect_uri)
	);
	const oidcLocked = $derived(!oidc.testPassed && !oidc.saved?.enabled);
	const oidcToggleHelp = $derived(
		!hasOidcCredentials
			? 'Enter a client ID and redirect URI to enable SSO.'
			: oidcLocked
				? 'Test the connection first.'
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

	async function verifyLocalFile(draft: SecuritySettings) {
		verifying = true;
		verifyResult = null;
		try {
			verifyResult = await api.v3.POST(
				SETTINGS_ENDPOINTS.securityVerifyHibp(),
				$state.snapshot(draft)
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

	const HSTS_PRESETS = [
		{ label: 'Disabled', value: 0 },
		{ label: '1 month', value: 30 * 24 * 3600 },
		{ label: '6 months', value: 180 * 24 * 3600 },
		{ label: '1 year', value: 365 * 24 * 3600 }
	];

	function resetToDefaults(draft: SecuritySettings) {
		Object.assign(draft, DEFAULTS, { frame_ancestors: [] });
	}
</script>

<div class="space-y-6">
	<div>
		<h2 class="text-2xl font-bold">Security</h2>
		<p class="text-base-content/60 mt-1">
			Configure password security and HTTPS enforcement headers.
		</p>
	</div>

	<QueryBoundary query={security} what="the security settings">
		{#snippet children(draft)}
			<div class="space-y-6">
				<!-- Password Security -->
				<Card>
					{#snippet header()}
						<div class="flex items-center gap-2">
							<div class="bg-primary/10 rounded-lg p-2">
								<KeyRound class="h-5 w-5 text-primary" />
							</div>
							<div>
								<h3 class="font-semibold">Password Security</h3>
								<p class="text-xs text-base-content/50">
									Controls applied when accounts are created.
								</p>
							</div>
						</div>
					{/snippet}
					<div class="mt-2 flex flex-col gap-4">
						<ToggleRow
							label="Check passwords against Have I Been Pwned"
							description="Rejects passwords found in known data breaches. Uses k-anonymity, only the first 5 characters of the password's SHA-1 hash are sent. The full password is never transmitted."
							bind:checked={draft.hibp_check}
						/>

						{#if !draft.hibp_check}
							<div class="alert alert-warning alert-soft text-sm">
								<CircleAlert class="h-4 w-4 shrink-0" />
								<span
									>HIBP checking is disabled. Users may set passwords from known breach databases.
									Only turn this off for air-gapped or offline installs.</span
								>
							</div>
						{/if}

						{#if draft.hibp_check}
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
												bind:value={draft.hibp_local_path}
												oninput={() => (verifyResult = null)}
											/>
											<Button
												variant="ghost"
												icon={FolderSearch}
												class="shrink-0"
												loading={verifying}
												disabled={!draft.hibp_local_path.trim()}
												onclick={() => void verifyLocalFile(draft)}
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
									>. When set and the file exists, no outbound API calls are made. If the path is
									set but the file is missing, the check is skipped rather than falling back to the
									API.
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

							{#if !draft.hibp_local_path.trim()}
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
									Who can download library files and full-album archives. Streaming is never
									restricted.
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
									checked={draft.library_download_access === option.value}
									onchange={() => (draft.library_download_access = option.value)}
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
								<strong>Only enable if nothing on this hostname is reached over plain HTTP.</strong> A
								browser that has seen the header refuses plain HTTP to this hostname, on every port, for
								as long as Max-Age says. That covers any other service on the same hostname too. The header
								is only sent on requests that arrive over HTTPS.
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
											bind:value={draft.hsts_max_age}
										/>
										<div class="flex gap-1 flex-wrap">
											{#each HSTS_PRESETS as preset (preset.value)}
												<Button
													size="xs"
													variant={draft.hsts_max_age === preset.value ? 'primary' : 'ghost'}
													aria-pressed={draft.hsts_max_age === preset.value}
													onclick={() => (draft.hsts_max_age = preset.value)}
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
							bind:checked={draft.hsts_include_subdomains}
							disabled={draft.hsts_max_age === 0}
						/>

						<div>
							<ToggleRow
								label="Preload"
								description="Adds the preload directive. Very difficult to undo."
								bind:checked={draft.hsts_preload}
								disabled={draft.hsts_max_age === 0}
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

						{#if draft.hsts_preload && !draft.hsts_include_subdomains}
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
									Tells the browser what the web UI may load and run, and who may show it in a
									frame.
								</p>
							</div>
						</div>
					{/snippet}
					<div class="mt-2 flex flex-col gap-4">
						<SelectField
							label="Policy"
							bind:value={draft.content_security_policy}
							options={POLICY_MODES}
							help={POLICY_MODE_HELP[draft.content_security_policy]}
						/>

						<p class="text-sm text-base-content/70">
							A reverse proxy that rewrites the page can leave the web UI blank while the policy is
							enforced. If that happens, stop the server and run
							<code class="text-xs">droppedneedle-tool content-security-policy report-only</code> on the
							host.
						</p>

						<ListField
							label="Allowed to frame DroppedNeedle"
							itemLabel="Address"
							addLabel="Add an address"
							bind:values={draft.frame_ancestors}
							max={MAX_FRAMING_ADDRESSES}
							mono
							placeholder="https://home.example.com"
							empty="No address listed: nobody may show DroppedNeedle in a frame."
							help="Each address listed here, such as a dashboard at https://home.example.com, may show DroppedNeedle in a frame unless the policy is off. A dashboard on the same site (another subdomain of the same domain, or the same host on another port) works; one on another site shows the sign-in page, because the browser does not send the sign-in cookie there. The scheme, host and port, no path. Up to {MAX_FRAMING_ADDRESSES}."
							disabled={security.saving}
						/>
					</div>
				</Card>

				<div class="flex flex-wrap items-center gap-3">
					<Button
						variant="ghost"
						size="sm"
						icon={RotateCcw}
						onclick={() => resetToDefaults(draft)}
						disabled={security.saving}
					>
						Reset to defaults
					</Button>
					<div class="min-w-0 flex-1">
						<SaveBar
							{...security.bar}
							disabled={draft.hsts_preload && !draft.hsts_include_subdomains}
						/>
					</div>
				</div>
			</div>
		{/snippet}
	</QueryBoundary>

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
			<QueryBoundary query={oidc} what="the single sign-on settings">
				{#snippet children(draft)}
					<div class="flex flex-col gap-4">
						<TextField
							label="Issuer URL"
							type="url"
							placeholder="https://accounts.example.com"
							help="The base URL of your OIDC provider. Must serve a /.well-known/openid-configuration document."
							bind:value={draft.issuer}
						/>

						<TextField label="Client ID" bind:value={draft.client_id} />

						<SecretField
							label="Client Secret"
							mask={SECRET_MASK}
							help="Optional - leave blank for public/PKCE clients"
							bind:value={draft.client_secret}
						/>

						<TextField
							label="Redirect URI"
							type="url"
							placeholder={`https://droppedneedle.example.com${AUTH_ENDPOINTS.oidcCallback}`}
							help={`This must point to the DroppedNeedle backend API (it ends in ${AUTH_ENDPOINTS.oidcCallback}), not the web app page. Register this exact URL as a redirect/callback URI with your provider.`}
							bind:value={draft.redirect_uri}
						/>

						<TextField
							label="Scopes"
							placeholder="openid email profile"
							bind:value={draft.scopes}
						/>

						<ToggleRow
							label="Allow login with SSO"
							description={oidcToggleHelp}
							bind:checked={draft.enabled}
							disabled={!hasOidcCredentials || oidcLocked}
						/>

						<SaveBar {...oidc.bar} testLabel="Test connection" />
					</div>
				{/snippet}
			</QueryBoundary>
		</div>
	</Card>
</div>
