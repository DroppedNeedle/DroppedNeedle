<script lang="ts">
	import SettingsPaneHeader from './SettingsPaneHeader.svelte';
	import { CircleCheck, CircleX, DatabaseBackup, TriangleAlert } from 'lucide-svelte';
	import {
		createRunBackupMutation,
		createSaveBackupSettingsMutation,
		getBackupSettingsQuery,
		getBackupsQuery,
		getRestoreReportQuery,
		type BackupKind,
		type BackupView
	} from '$lib/features/settings/queries/system.svelte';
	import { docsUrl } from '$lib/ui/docsUrl';
	import { outsideLink } from '$lib/ui/outsideLink';
	import { toastStore } from '$lib/stores/toast';
	import { formatBytes } from '$lib/utils/formatting';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';
	import SaveBar from '$lib/ui/SaveBar.svelte';
	import TextField from '$lib/ui/TextField.svelte';
	import ToggleRow from '$lib/ui/ToggleRow.svelte';
	import { createSettingsSection } from '$lib/ui/settingsSection.svelte';

	const RESTORE_DOCS = docsUrl('backups-and-restore', 'restoring-a-backup');
	const STORAGE_DOCS = docsUrl('configuration', 'storage-and-permissions');

	const backupsQuery = getBackupsQuery();
	const runBackup = createRunBackupMutation();
	const settings = createSettingsSection(
		getBackupSettingsQuery(),
		createSaveBackupSettingsMutation()
	);

	const KIND_LABELS: Record<BackupKind, string> = {
		scheduled: 'Nightly',
		manual: 'By hand',
		pre_upgrade: 'Before upgrade'
	};

	let checking = $state<string | null>(null);
	let checkOpen = $state(false);
	const reportQuery = getRestoreReportQuery(() => (checkOpen ? checking : null));

	// Newest first; the server lists oldest first.
	const newestFirst = (backups: BackupView[]) => [...backups].reverse();

	function formatDate(unixSeconds: number): string {
		return new Date(unixSeconds * 1000).toLocaleString(undefined, {
			dateStyle: 'medium',
			timeStyle: 'short'
		});
	}

	function details(backup: BackupView): string {
		return [
			KIND_LABELS[backup.kind],
			backup.created_at_unix != null ? formatDate(backup.created_at_unix) : null,
			formatBytes(backup.size_bytes),
			backup.user_version != null ? `Schema ${backup.user_version}` : null
		]
			.filter(Boolean)
			.join(' · ');
	}

	async function backUpNow() {
		try {
			const run = await runBackup.mutateAsync();
			toastStore.show({
				message: `Backed up: ${run.name} (${formatBytes(run.size_bytes)})`,
				type: 'success'
			});
		} catch (error) {
			toastStore.show({
				message: error instanceof Error && error.message ? error.message : 'Backup failed',
				type: 'error'
			});
		}
	}

	function openCheck(name: string) {
		checking = name;
		checkOpen = true;
	}
</script>

<div class="space-y-5">
	<SettingsPaneHeader title="Backups" description="Copies of the database, settings included.">
		{#if backupsQuery.data}
			{@const keep = backupsQuery.data.keep}
			<p>
				Kept: {keep.daily} daily, {keep.weekly} weekly, {keep.pre_upgrade} before an upgrade,
				{keep.manual} by hand.
			</p>
		{/if}
		{#snippet actions()}
			<AsyncButton variant="primary" size="display-sm" icon={DatabaseBackup} onclick={backUpNow}>
				Back up now
			</AsyncButton>
		{/snippet}
	</SettingsPaneHeader>
	{#if backupsQuery.data?.database_in_memory}
		<div class="alert alert-warning items-start" role="status">
			<TriangleAlert class="mt-0.5 h-5 w-5" aria-hidden="true" />
			<div class="min-w-0 flex-1 space-y-1 text-sm">
				<strong>Database in memory</strong>
				<p>
					It and its backups are lost on restart. Map /app/cache to a disk or pool path.
					<a
						class="link whitespace-nowrap"
						href={outsideLink(STORAGE_DOCS)}
						target="_blank"
						rel="noopener noreferrer"
					>
						How to move it
					</a>
				</p>
			</div>
		</div>
	{/if}
	<Card variant="bordered" padding="md">
		<QueryBoundary query={settings} what="the nightly backup setting">
			{#snippet children(draft)}
				<div class="space-y-4">
					<ToggleRow
						label="Back up every night"
						description="Once a day, while the server runs."
						bind:checked={draft.nightly}
						disabled={settings.saving}
					/>
					<TextField
						label="Second folder"
						help="Optional. Each backup is also copied here. Use an empty folder on another disk. The key file is not copied, so keep it safe yourself."
						placeholder="/backups"
						mono
						bind:value={draft.second_folder}
						error={settings.fieldError('second_folder')}
						disabled={settings.saving}
					/>
					<SaveBar {...settings.bar} />
				</div>
			{/snippet}
		</QueryBoundary>
	</Card>
	<Card variant="bordered" padding="md">
		<QueryBoundary
			query={backupsQuery}
			what="the backups"
			isEmpty={(data) => data.backups.length === 0}
		>
			{#snippet empty()}
				<p class="py-4 text-sm text-muted">No backups yet.</p>
			{/snippet}
			{#snippet children(data)}
				<ul class="divide-y divide-line">
					{#each newestFirst(data.backups) as backup (backup.name)}
						<li class="flex items-center gap-3 py-3 first:pt-0">
							<div class="min-w-0 flex-1">
								<p class="font-mono text-xs break-all sm:text-sm">{backup.name}</p>
								<p class="mt-0.5 text-xs text-muted">{details(backup)}</p>
							</div>
							<Button
								variant="outline"
								size="sm"
								aria-label="Check {backup.name}"
								onclick={() => openCheck(backup.name)}
							>
								Check
							</Button>
						</li>
					{/each}
				</ul>
			{/snippet}
		</QueryBoundary>

		<p class="mt-4 text-xs text-muted">
			Restoring runs on the host with the server stopped.
			<a
				class="link whitespace-nowrap"
				href={outsideLink(RESTORE_DOCS)}
				target="_blank"
				rel="noopener noreferrer"
			>
				How to restore
			</a>
		</p>
	</Card>
</div>

<Modal bind:open={checkOpen} title="Check backup" description={checking ?? undefined}>
	<QueryBoundary query={reportQuery} what="the check">
		{#snippet children(report)}
			<p class={['mb-3 text-sm font-medium', report.restorable ? 'text-success' : 'text-error']}>
				{report.restorable ? 'This backup can be restored.' : 'This backup cannot be restored.'}
			</p>
			<ul class="space-y-2">
				{#each report.checks as check (check.name)}
					<li class="flex items-start gap-2 text-sm">
						{#if check.passed}
							<CircleCheck class="mt-0.5 h-4 w-4 shrink-0 text-success" aria-label="Passed" />
						{:else}
							<CircleX class="mt-0.5 h-4 w-4 shrink-0 text-error" aria-label="Failed" />
						{/if}
						<div class="min-w-0">
							<p class="font-mono text-xs">{check.name}</p>
							<p class="text-muted">{check.detail}</p>
						</div>
					</li>
				{/each}
			</ul>
		{/snippet}
	</QueryBoundary>
</Modal>
