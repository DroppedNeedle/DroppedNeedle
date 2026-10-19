<script lang="ts">
	import {
		CircleCheck,
		CircleX,
		FolderTree,
		HardDriveDownload,
		Info,
		TriangleAlert
	} from 'lucide-svelte';

	import {
		getDownloadClientConfigQuery,
		getDownloadClientStatusQuery,
		saveDownloadClientConfig,
		testDownloadClient
	} from '$lib/queries/downloads/DownloadClientQueries.svelte';
	import type {
		DownloadClientConfig,
		TestConnectionResult
	} from '$lib/queries/downloads/DownloadClientQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const configQuery = getDownloadClientConfigQuery();
	const statusQuery = getDownloadClientStatusQuery();
	const save = saveDownloadClientConfig();
	const test = testDownloadClient();

	let enabled = $state(false);
	let url = $state('');
	let apiKey = $state('');
	let downloadsSubpath = $state('');
	let incompleteMount = $state('');
	let seeded = $state(false);
	let testResult = $state<TestConnectionResult | null>(null);

	$effect(() => {
		const d = configQuery.data;
		if (d && !seeded) {
			enabled = d.enabled;
			url = d.url;
			apiKey = d.api_key;
			downloadsSubpath = d.downloads_subpath ?? '';
			incompleteMount = d.slskd_incomplete_mount ?? '';
			seeded = true;
		}
	});

	const status = $derived(statusQuery.data);
	const connected = $derived(status?.configured === true && status?.client.status === 'ok');
	const statusText = $derived(
		connected
			? `Connected${status?.client.version ? ` · v${status.client.version}` : ''}`
			: status?.configured
				? (status?.client.message ?? 'Not reachable')
				: 'Not configured'
	);
	const mount = $derived(status?.mount);
	const mountAdvisory = $derived(status?.mount_advisory);
	const slskdDownloadsDir = $derived(status?.slskd_downloads_dir);

	// Mirror of the backend join (mount + sanitised subpath): the folder DroppedNeedle
	// actually looks in. Live from the form so typing updates it; server truth until seeded.
	function joinSubpath(mountPath: string, subpath: string): string {
		const parts = subpath
			.split(/[\\/]/)
			.map((p) => p.trim())
			.filter((p) => p && p !== '.' && p !== '..');
		return parts.length ? `${mountPath.replace(/\/+$/, '')}/${parts.join('/')}` : mountPath;
	}

	const liveEffectivePath = $derived(
		mount?.path
			? seeded
				? joinSubpath(mount.path, downloadsSubpath)
				: (status?.effective_downloads_path ?? mount.path)
			: null
	);
	const mountAlreadyPointsAtSlskd = $derived(
		!!slskdDownloadsDir && !!mount?.path && slskdDownloadsDir === mount.path
	);
	const subpathLooksAbsolute = $derived(downloadsSubpath.trim().startsWith('/'));

	function trimSubpathToRelative(): void {
		const mountBase = mount?.path?.replace(/\/+$/, '') ?? '';
		let rest = downloadsSubpath.replace(/\\/g, '/');
		if (mountBase && rest.includes(mountBase)) {
			rest = rest.slice(rest.indexOf(mountBase) + mountBase.length);
		}
		downloadsSubpath = rest
			.split('/')
			.map((p) => p.trim())
			.filter((p) => p && p !== '.' && p !== '..')
			.join('/');
	}

	const MOUNT_REASONS: Record<string, string> = {
		not_set: 'No slskd downloads folder is mounted into DroppedNeedle.',
		missing: "The mounted downloads folder doesn't exist.",
		not_writable:
			'The downloads mount is read-only - imports must remove source files after they are placed.'
	};

	function currentConfig(): DownloadClientConfig | null {
		const d = configQuery.data;
		if (!d) return null;
		return {
			...d,
			enabled,
			url,
			api_key: apiKey,
			downloads_subpath: downloadsSubpath,
			slskd_incomplete_mount: incompleteMount
		};
	}

	async function handleTest() {
		testResult = null;
		const config = currentConfig();
		if (!config) return;
		try {
			testResult = await test.mutateAsync(config);
		} catch (e) {
			testResult = { valid: false, message: e instanceof Error ? e.message : 'Connection failed' };
		}
	}

	async function handleSave() {
		const config = currentConfig();
		if (!config) return;
		try {
			await save.mutateAsync(config);
			toastStore.show({ message: 'Soulseek settings saved', type: 'success' });
		} catch (e) {
			toastStore.show({
				message: e instanceof Error ? e.message : 'Failed to save download client',
				type: 'error'
			});
		}
	}

	// The enable switch persists immediately (it lives in the collapsed header, so users
	// shouldn't have to expand + Save just to flip a source on/off). Revert on failure.
	async function handleToggle() {
		const config = currentConfig();
		if (!config) return;
		try {
			await save.mutateAsync(config);
			toastStore.show({ message: `Soulseek ${enabled ? 'enabled' : 'disabled'}`, type: 'success' });
		} catch {
			enabled = !enabled;
			toastStore.show({ message: 'Could not update Soulseek', type: 'error' });
		}
	}
</script>

{#if configQuery.isLoading}
	<div class="skeleton h-28 w-full rounded-box"></div>
{:else if configQuery.isError}
	<div class="alert alert-error">
		Failed to load Soulseek settings: {configQuery.error.message}
	</div>
{:else}
	<DownloadClientCard
		title="slskd"
		sourceLabel="Soulseek"
		icon={HardDriveDownload}
		{connected}
		{statusText}
		bind:enabled
		onToggle={handleToggle}
		enableAriaLabel="Enable slskd download client"
	>
		<section class="space-y-3">
			<TextField label="slskd URL" mono bind:value={url} placeholder="http://slskd:5030" />
			<SecretField
				label="API key"
				mask={configQuery.data?.api_key || undefined}
				bind:value={apiKey}
				placeholder="slskd API key"
			/>
			<div class="flex flex-wrap items-center gap-3">
				<Button
					variant="outline"
					size="sm"
					loading={test.isPending}
					disabled={!url}
					onclick={handleTest}
				>
					Test connection
				</Button>
				{#if testResult}
					<span
						class="flex items-center gap-1.5 text-sm"
						class:text-success={testResult.valid}
						class:text-error={!testResult.valid}
					>
						{#if testResult.valid}
							<CircleCheck class="size-4" aria-hidden="true" /> Connected{testResult.version
								? ` · v${testResult.version}`
								: ''}
						{:else}
							<CircleX class="size-4" aria-hidden="true" /> {testResult.message}
						{/if}
					</span>
				{/if}
			</div>
		</section>

		<p class="text-xs leading-relaxed text-base-content/60">
			DroppedNeedle only orchestrates your own slskd instance over its local HTTP API; it never
			joins or distributes on the Soulseek network. You supply, run, and are responsible for slskd
			and its shared folders.
		</p>

		<div class="alert alert-warning items-start text-sm">
			<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
			<span>
				<span class="font-semibold">Share files in slskd.</span> Soulseek bans leechers - configure
				shared directories in <code>slskd.yml</code> so you can keep downloading.
			</span>
		</div>

		<section class="space-y-2">
			<div class="flex items-center gap-2 text-sm font-semibold">
				<FolderTree class="size-4 text-base-content/70" aria-hidden="true" /> Downloads mount
			</div>
			{#if liveEffectivePath}
				<p class="text-xs text-base-content/60">
					DroppedNeedle looks in
					<code class="text-base-content/70" data-testid="effective-downloads-path"
						>{liveEffectivePath}</code
					>. The mount itself comes from <code>SLSKD_DOWNLOADS_PATH</code> in docker-compose. Recreate
					the container after changing it.
				</p>
			{/if}
			<!-- The subpath editor also renders before the first mount probe has
				run, so the setting stays editable. -->
			{#if (mount?.ok && mountAdvisory) || !mount}
				{#if mountAdvisory}
					<div class="alert alert-warning items-start text-sm">
						<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
						<div class="space-y-1">
							<p>{mountAdvisory}</p>
							{#if mount?.path}<code class="text-base-content/60">{mount?.path}</code>{/if}
						</div>
					</div>
				{/if}
				<Card variant="bordered" padding="sm">
					<div class="space-y-1.5">
						<label class="text-sm font-medium" for="downloads-subpath">
							Downloads subfolder
							<span class="badge badge-ghost badge-xs ml-1 align-middle">relative to the mount</span
							>
						</label>
						<p class="text-xs text-base-content/60">
							The folder inside your mount where slskd saves completed downloads.
							{#if slskdDownloadsDir}
								slskd saves to <code class="text-base-content/70">{slskdDownloadsDir}</code>. Set
								this to the part of that path inside your mount.
							{/if}
						</p>
						{#if mountAlreadyPointsAtSlskd && !downloadsSubpath.trim()}
							<p class="text-xs text-success">
								Leave this empty. Your mount already points where slskd saves.
							</p>
						{/if}
						{#if subpathLooksAbsolute}
							<div class="alert alert-warning items-start py-2 text-xs">
								<TriangleAlert class="size-4 shrink-0" aria-hidden="true" />
								<div class="space-y-1.5">
									<p>
										This looks like a full path. Type only the part after the mount, or leave it
										empty.
									</p>
									<Button size="xs" onclick={trimSubpathToRelative}>Trim to relative</Button>
								</div>
							</div>
						{/if}
						<div class="flex flex-wrap items-center gap-2">
							<Input
								id="downloads-subpath"
								label="Downloads subfolder"
								size="sm"
								mono
								class="flex-1"
								placeholder="e.g. downloads/slskd/complete"
								bind:value={downloadsSubpath}
							/>
							<Button variant="primary" size="sm" onclick={handleSave} disabled={save.isPending}>
								{save.isPending ? 'Checking…' : 'Save & re-check'}
							</Button>
						</div>
					</div>
				</Card>
			{:else if mount?.ok && mount.move_supported}
				<div class="flex items-center gap-2 text-sm text-success">
					<CircleCheck class="size-4" aria-hidden="true" />
					Fast move available
					{#if mount.path}<code class="text-base-content/60">{mount.path}</code>{/if}
				</div>
			{:else if mount?.reason === 'different_mount' || mount?.reason === 'different_filesystem'}
				<div class="alert alert-info items-start text-sm">
					<Info class="size-5 shrink-0" aria-hidden="true" />
					<div class="space-y-1">
						<p>
							Your downloads and library use separate container mount boundaries. DroppedNeedle will
							copy each file into the library and remove the source after the copy succeeds. This is
							slower and temporarily needs room for both copies.
						</p>
						<p class="text-base-content/70">
							For fast moves, expose both folders through one common-parent container mount.
							{#if mount.path}<code class="text-base-content/60">{mount.path}</code>{/if}
						</p>
					</div>
				</div>
			{:else if mount?.reason === 'stat_error'}
				<div class="alert alert-info items-start text-sm">
					<Info class="size-5 shrink-0" aria-hidden="true" />
					<p>
						The downloads path is writable, but DroppedNeedle couldn't determine whether a fast move
						is available. Imports will try the move first and copy when needed.
					</p>
				</div>
			{:else if mount?.ok}
				<div class="alert alert-info items-start text-sm">
					<Info class="size-5 shrink-0" aria-hidden="true" />
					<p>
						The downloads path is ready. Configure a library root before DroppedNeedle can check
						whether fast moves are available.
					</p>
				</div>
			{:else if mount}
				<div class="alert alert-warning items-start text-sm">
					<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
					<div class="space-y-1">
						<p>
							DroppedNeedle can't reach slskd's downloads folder, so finished downloads won't
							import.
							{MOUNT_REASONS[mount.reason] ?? mount.reason}
						</p>
						<details class="text-xs">
							<summary class="cursor-pointer font-semibold">How to set this up</summary>
							<p class="mt-1 text-base-content/70">
								Expose slskd's completed-downloads directory to DroppedNeedle
								<strong>read-write</strong>. For fast moves, keep it and the library inside one
								common-parent container mount. See the slskd setup section in the README.
							</p>
						</details>
					</div>
				</div>
			{/if}
		</section>

		<Card variant="bordered" padding="sm" as="section">
			<div class="space-y-1.5">
				<label class="text-sm font-medium" for="incomplete-mount">
					Incomplete folder (optional)
					<span class="badge badge-ghost badge-xs ml-1 align-middle">absolute path</span>
				</label>
				<p class="text-xs text-base-content/60">
					The absolute container path where slskd keeps unfinished downloads. When set,
					DroppedNeedle spots stranded partial bytes there and retries the missing file instead of
					reporting it lost. Leave empty to disable.
				</p>
				<div class="flex flex-wrap items-center gap-2">
					<Input
						id="incomplete-mount"
						label="Incomplete folder (optional)"
						size="sm"
						mono
						class="flex-1"
						placeholder="e.g. /data/slskd/incomplete"
						bind:value={incompleteMount}
					/>
					<Button variant="primary" size="sm" onclick={handleSave} disabled={save.isPending}>
						{save.isPending ? 'Checking…' : 'Save & re-check'}
					</Button>
				</div>
			</div>
		</Card>

		<div class="flex justify-end">
			<Button variant="primary" loading={save.isPending} onclick={handleSave}>Save settings</Button>
		</div>
	</DownloadClientCard>
{/if}
