<script lang="ts">
	import { CircleCheck, FolderTree, HardDriveDownload, Info, TriangleAlert } from 'lucide-svelte';

	import {
		getDownloadClientConfigQuery,
		getDownloadClientStatusQuery,
		saveDownloadClientConfig,
		testDownloadClient
	} from '$lib/queries/downloads/DownloadClientQueries.svelte';
	import type { DownloadClientConfig } from '$lib/queries/downloads/DownloadClientQueries.svelte';
	import { toastStore } from '$lib/stores/toast';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Input from '$lib/ui/Input.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import SecretField from '$lib/ui/SecretField.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import { SECRET_MASK } from '$lib/ui/secretMask';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	import DownloadClientCard from './DownloadClientCard.svelte';

	const statusQuery = getDownloadClientStatusQuery();
	const test = testDownloadClient();

	const section = createSettingsSection(
		getDownloadClientConfigQuery(),
		saveDownloadClientConfig(),
		{
			test: async (draft) => {
				if (!draft.url.trim()) return { passed: false, message: 'Add the URL first.' };
				const result = await test.mutateAsync(draft);
				return {
					passed: result.valid,
					message: result.valid
						? `Connected${result.version ? ` · v${result.version}` : ''}`
						: result.message
				};
			}
		}
	);

	const status = $derived(statusQuery.data);
	const connected = $derived(status?.configured === true && status?.reachable === true);
	const statusText = $derived(
		connected
			? `Connected${status?.version ? ` · v${status.version}` : ''}`
			: status?.configured
				? (status?.message ?? 'Not reachable')
				: 'Not configured'
	);
	const mount = $derived(status?.mount);
	const mountAdvisory = $derived(status?.mount_advisory);
	const slskdDownloadsDir = $derived(status?.slskd_downloads_dir);

	// Mirror of the backend join (mount + sanitised subpath): the folder DroppedNeedle
	// actually looks in. Live from the form so typing updates it.
	function joinSubpath(mountPath: string, subpath: string): string {
		const parts = subpath
			.split(/[\\/]/)
			.map((p) => p.trim())
			.filter((p) => p && p !== '.' && p !== '..');
		return parts.length ? `${mountPath.replace(/\/+$/, '')}/${parts.join('/')}` : mountPath;
	}

	function trimSubpathToRelative(draft: DownloadClientConfig): void {
		const mountBase = mount?.path?.replace(/\/+$/, '') ?? '';
		let rest = (draft.downloads_subpath ?? '').replace(/\\/g, '/');
		if (mountBase && rest.includes(mountBase)) {
			rest = rest.slice(rest.indexOf(mountBase) + mountBase.length);
		}
		draft.downloads_subpath = rest
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

	// The enable switch sits in the collapsed header, so it saves at once rather
	// than waiting for Save. It is switched back if the save fails.
	async function onToggle(draft: DownloadClientConfig) {
		await section.save();
		if (section.status?.tone === 'success') {
			toastStore.show({
				message: `Soulseek ${draft.enabled ? 'enabled' : 'disabled'}`,
				type: 'success'
			});
		} else {
			draft.enabled = !draft.enabled;
			toastStore.show({ message: 'Could not update Soulseek', type: 'error' });
		}
	}
</script>

<QueryBoundary query={section} what="the Soulseek settings">
	{#snippet loading()}
		<div class="skeleton h-28 w-full rounded-box"></div>
	{/snippet}
	{#snippet children(draft)}
		<DownloadClientCard
			title="slskd"
			sourceLabel="Soulseek"
			icon={HardDriveDownload}
			{connected}
			{statusText}
			bind:enabled={draft.enabled}
			onToggle={() => void onToggle(draft)}
			enableAriaLabel="Enable slskd download client"
		>
			<section class="space-y-3">
				<TextField label="slskd URL" mono bind:value={draft.url} placeholder="http://slskd:5030" />
				<SecretField
					label="API key"
					mask={SECRET_MASK}
					bind:value={draft.api_key}
					placeholder="slskd API key"
				/>
			</section>

			<p class="text-xs leading-relaxed text-base-content/60">
				DroppedNeedle only orchestrates your own slskd instance over its local HTTP API; it never
				joins or distributes on the Soulseek network. You supply, run, and are responsible for slskd
				and its shared folders.
			</p>

			<div class="alert alert-warning items-start text-sm">
				<TriangleAlert class="size-5 shrink-0" aria-hidden="true" />
				<span>
					<span class="font-semibold">Share files in slskd.</span> Soulseek bans leechers -
					configure shared directories in <code>slskd.yml</code> so you can keep downloading.
				</span>
			</div>

			<section class="space-y-2">
				<div class="flex items-center gap-2 text-sm font-semibold">
					<FolderTree class="size-4 text-base-content/70" aria-hidden="true" /> Downloads mount
				</div>
				{#if mount?.path}
					<p class="text-xs text-base-content/60">
						DroppedNeedle looks in
						<code class="text-base-content/70" data-testid="effective-downloads-path"
							>{joinSubpath(mount.path, draft.downloads_subpath ?? '')}</code
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
								<span class="badge badge-ghost badge-xs ml-1 align-middle"
									>relative to the mount</span
								>
							</label>
							<p class="text-xs text-base-content/60">
								The folder inside your mount where slskd saves completed downloads.
								{#if slskdDownloadsDir}
									slskd saves to <code class="text-base-content/70">{slskdDownloadsDir}</code>. Set
									this to the part of that path inside your mount.
								{/if}
							</p>
							{#if slskdDownloadsDir && slskdDownloadsDir === mount?.path && !draft.downloads_subpath?.trim()}
								<p class="text-xs text-success">
									Leave this empty. Your mount already points where slskd saves.
								</p>
							{/if}
							{#if draft.downloads_subpath?.trim().startsWith('/')}
								<div class="alert alert-warning items-start py-2 text-xs">
									<TriangleAlert class="size-4 shrink-0" aria-hidden="true" />
									<div class="space-y-1.5">
										<p>
											This looks like a full path. Type only the part after the mount, or leave it
											empty.
										</p>
										<Button size="xs" onclick={() => trimSubpathToRelative(draft)}
											>Trim to relative</Button
										>
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
									bind:value={draft.downloads_subpath}
								/>
								<Button
									variant="primary"
									size="sm"
									onclick={() => void section.save()}
									disabled={section.saving}
								>
									{section.saving ? 'Checking…' : 'Save & re-check'}
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
								Your downloads and library use separate container mount boundaries. DroppedNeedle
								will copy each file into the library and remove the source after the copy succeeds.
								This is slower and temporarily needs room for both copies.
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
							The downloads path is writable, but DroppedNeedle couldn't determine whether a fast
							move is available. Imports will try the move first and copy when needed.
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
							bind:value={draft.slskd_incomplete_mount}
						/>
						<Button
							variant="primary"
							size="sm"
							onclick={() => void section.save()}
							disabled={section.saving}
						>
							{section.saving ? 'Checking…' : 'Save & re-check'}
						</Button>
					</div>
				</div>
			</Card>

			<SaveBar {...section.bar} testLabel="Test connection" />
		</DownloadClientCard>
	{/snippet}
</QueryBoundary>
