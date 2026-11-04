<script lang="ts">
	import '$lib/shell/release-notes.css';
	import { goto } from '$app/navigation';
	import { isWhatsNewDismissed, dismissWhatsNew } from '$lib/stores/version.svelte';
	import SafeHtml from '$lib/ui/SafeHtml.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Modal from '$lib/ui/Modal.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { Sparkles, ExternalLink } from 'lucide-svelte';
	import type { GitHubRelease } from '$lib/shell/version.svelte';

	interface Props {
		currentVersion: string | null;
		buildDate: string | null;
		releases: GitHubRelease[];
	}
	let { currentVersion, buildDate, releases }: Props = $props();

	let open = $state(false);

	const isDev = $derived(currentVersion === 'dev');
	const latestRelease = $derived(releases.length > 0 ? releases[0] : null);

	// In dev: key dismissal to build_date so modal shows once per rebuild, not every refresh
	// In prod: key to latest release tag so modal re-shows when a new patch lands
	const dismissKey = $derived(
		isDev ? (buildDate ?? 'dev') : (latestRelease?.tag_name ?? currentVersion)
	);

	const hasContent = $derived(releases.some((r) => r.body && r.body.trim().length > 0));

	const shouldShow = $derived(
		currentVersion !== null && dismissKey !== null && hasContent && !isWhatsNewDismissed(dismissKey)
	);

	const sections = $derived(
		releases.flatMap((r) =>
			r.body && r.body.trim() ? [{ tag: r.tag_name, name: r.name ?? null, body: r.body }] : []
		)
	);

	$effect(() => {
		if (shouldShow) open = true;
	});

	// However it closes, this release has been seen.
	function dismiss() {
		if (dismissKey) dismissWhatsNew(dismissKey);
	}

	function close() {
		dismiss();
		open = false;
	}

	function handleViewChangelog() {
		close();
		goto(withBasePath('/settings?tab=about'));
	}
</script>

<Modal
	bind:open
	title="What's New"
	description={currentVersion ?? undefined}
	icon={Sparkles}
	size="lg"
	closeButton
	onclose={dismiss}
>
	{#if sections.length > 0}
		<div
			class="whats-new-content release-notes-prose max-h-[55vh] overflow-y-auto rounded-lg border border-base-content/5 bg-base-200/50 p-4"
		>
			{#each sections as section, i (section.tag)}
				{#if i > 0}
					<div class="divider my-4 opacity-20"></div>
				{/if}
				<p class="mb-3 border-l-2 border-accent/50 pl-3 text-sm font-semibold text-base-content">
					{section.name ?? section.tag}
				</p>
				<SafeHtml markdown={section.body} class="prose prose-sm max-w-none text-base-content/75" />
			{/each}
		</div>
	{/if}

	{#snippet actions()}
		<Button
			variant="ghost"
			size="sm"
			icon={ExternalLink}
			iconPosition="end"
			onclick={handleViewChangelog}
		>
			View full changelog
		</Button>
		<Button variant="accent" size="sm" onclick={close}>Got it</Button>
	{/snippet}
</Modal>

<style>
	.whats-new-content {
		scrollbar-width: thin;
		scrollbar-color: oklch(from var(--color-accent) l c h / 0.15) transparent;
	}
</style>
