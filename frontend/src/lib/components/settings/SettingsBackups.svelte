<script lang="ts">
	import { CircleCheck, CircleX, DatabaseBackup } from 'lucide-svelte';
	import {
		createRunBackupMutation,
		getBackupsQuery,
		getRestoreReportQuery,
		type BackupView
	} from '$lib/queries/system/BackupQueries.svelte';
	import { outsideLink } from '$lib/components/outsideLink';
	import { toastStore } from '$lib/stores/toast';
	import { formatBytes } from '$lib/utils/formatting';
	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import CardTitle from '$lib/ui/CardTitle.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import QueryBoundary from '$lib/ui/QueryBoundary.svelte';

	const RESTORE_DOCS =
		'https://github.com/DroppedNeedle/DroppedNeedle/blob/v3/docs/reverse-proxy-and-security.md#backups';

	const backupsQuery = getBackupsQuery();
	const runBackup = createRunBackupMutation();

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

<Card>
	{#snippet header()}
		<div class="flex flex-wrap items-start justify-between gap-3">
			<div class="min-w-0">
				<CardTitle class="text-2xl">Backups</CardTitle>
				<p class="mt-1 text-sm text-base-content/70">
					Copies of the database, settings included.
					{#if backupsQuery.data}The newest {backupsQuery.data.keep} are kept.{/if}
				</p>
			</div>
			<AsyncButton variant="primary" size="sm" icon={DatabaseBackup} onclick={backUpNow}>
				Back up now
			</AsyncButton>
		</div>
	{/snippet}

	<QueryBoundary
		query={backupsQuery}
		what="the backups"
		isEmpty={(data) => data.backups.length === 0}
	>
		{#snippet empty()}
			<p class="py-4 text-sm text-base-content/60">No backups yet.</p>
		{/snippet}
		{#snippet children(data)}
			<ul class="divide-y divide-base-300/40">
				{#each newestFirst(data.backups) as backup (backup.name)}
					<li class="flex items-center gap-3 py-3">
						<div class="min-w-0 flex-1">
							<p class="font-mono text-xs break-all sm:text-sm">{backup.name}</p>
							<p class="mt-0.5 text-xs text-base-content/60">{details(backup)}</p>
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

	<p class="mt-4 text-xs text-base-content/60">
		Restoring runs on the host with the server stopped.
		<a
			class="link whitespace-nowrap"
			href={outsideLink(RESTORE_DOCS)}
			target="_blank"
			rel="noopener noreferrer">How to restore</a
		>
	</p>
</Card>

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
							<p class="text-base-content/70">{check.detail}</p>
						</div>
					</li>
				{/each}
			</ul>
		{/snippet}
	</QueryBoundary>
</Modal>
