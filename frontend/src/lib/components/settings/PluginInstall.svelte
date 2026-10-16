<script lang="ts">
	import { ShieldAlert } from 'lucide-svelte';
	import {
		installPreviewedPluginMutation,
		previewPluginInstallMutation,
		type PluginInstallPreview
	} from '$lib/queries/plugins/PluginInstallMutations.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Field from '$lib/ui/Field.svelte';
	import Input from '$lib/ui/Input.svelte';

	const preview = previewPluginInstallMutation();
	const install = installPreviewedPluginMutation();

	let repoUrl = $state('');
	let shown = $state<(PluginInstallPreview & { repository_url: string }) | null>(null);

	function submitPreview(event: SubmitEvent) {
		event.preventDefault();
		const url = repoUrl.trim();
		if (!url || preview.isPending) return;
		preview.mutate(
			{ repository_url: url },
			{ onSuccess: (result) => (shown = { ...result, repository_url: url }) }
		);
	}

	function confirmInstall() {
		if (!shown || install.isPending) return;
		install.mutate(shown, {
			onSuccess: () => {
				shown = null;
				repoUrl = '';
			}
		});
	}
</script>

<form class="mt-2 flex flex-wrap items-end gap-2" onsubmit={submitPreview}>
	<div class="min-w-0 flex-1">
		<Field label="Install from GitHub">
			{#snippet children(control)}
				<Input
					{...control}
					label="Install from GitHub"
					type="url"
					size="sm"
					class="w-full"
					placeholder="https://github.com/owner/repo"
					bind:value={repoUrl}
					disabled={preview.isPending || install.isPending}
				/>
			{/snippet}
		</Field>
	</div>
	<Button
		type="submit"
		variant="primary"
		size="sm"
		class="mb-1"
		disabled={preview.isPending || install.isPending || !repoUrl.trim()}
	>
		{preview.isPending ? 'Checking…' : 'Preview'}
	</Button>
</form>
<p class="text-xs text-base-content/45">
	Takes the latest release, or the version in the URL (a <code>/releases/tag/…</code>,
	<code>/tree/…</code> or <code>/commit/…</code> link). Nothing is written until you confirm, and nothing
	runs until you enable it.
</p>

{#if shown}
	<Card variant="bordered" padding="md" class="mt-2">
		<div>
			<div class="flex flex-wrap items-baseline gap-2">
				<h3 class="font-semibold">{shown.display_name}</h3>
				<span class="text-xs text-base-content/50">v{shown.version}</span>
				{#if shown.author}
					<span class="text-xs text-base-content/50">by {shown.author}</span>
				{/if}
			</div>
			{#if shown.description}
				<p class="mt-0.5 text-xs text-base-content/60">{shown.description}</p>
			{/if}
			<p class="mt-1 text-xs text-base-content/50">
				{shown.repository} · {shown.ref_kind === 'branch' && shown.reference === 'HEAD'
					? 'default branch'
					: `${shown.ref_kind} ${shown.reference}`} · commit
				<code>{shown.commit.slice(0, 12)}</code>
			</p>
			{#if shown.installed_version}
				<p class="mt-1 text-xs text-base-content/60">
					Replaces the installed version {shown.installed_version}. Its settings and data stay.
				</p>
			{/if}

			<h4 class="mt-3 text-xs font-semibold text-base-content/70">This plugin asks to</h4>
			<ul class="mt-1 list-disc space-y-0.5 pl-5 text-xs text-base-content/70">
				{#each shown.permissions as permission (permission)}
					<li>{permission}</li>
				{/each}
			</ul>

			<div
				class="mt-3 flex items-start gap-2 rounded-lg bg-warning/10 px-3 py-2 text-xs text-warning-content"
				role="alert"
			>
				<ShieldAlert class="mt-0.5 h-4 w-4 shrink-0 text-warning" aria-hidden="true" />
				<span>{shown.warning}</span>
			</div>

			<div class="mt-3 flex justify-end gap-2">
				<Button
					variant="ghost"
					size="xs"
					onclick={() => (shown = null)}
					disabled={install.isPending}
				>
					Cancel
				</Button>
				<Button variant="warning" size="xs" onclick={confirmInstall} disabled={install.isPending}>
					{install.isPending ? 'Installing…' : 'I trust it, install'}
				</Button>
			</div>
		</div>
	</Card>
{/if}
