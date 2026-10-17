<script lang="ts">
	import '$lib/components/release-notes.css';
	import { goto } from '$app/navigation';
	import { isWhatsNewDismissed, dismissWhatsNew } from '$lib/stores/version.svelte';
	import SafeHtml from '$lib/components/SafeHtml.svelte';
	import Button from '$lib/ui/Button.svelte';
	import IconButton from '$lib/ui/IconButton.svelte';
	import { withBasePath } from '$lib/utils/basePath';
	import { X, Sparkles, ExternalLink } from 'lucide-svelte';
	import type { GitHubRelease } from '$lib/queries/VersionQuery.svelte';

	interface Props {
		currentVersion: string | null;
		buildDate: string | null;
		releases: GitHubRelease[];
	}
	let { currentVersion, buildDate, releases }: Props = $props();

	let dialogEl: HTMLDialogElement | undefined = $state();

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
		if (shouldShow && dialogEl && !dialogEl.open) {
			dialogEl.showModal();
		}
	});

	function handleDismiss() {
		if (dismissKey) {
			dismissWhatsNew(dismissKey);
		}
		dialogEl?.close();
	}

	function onDialogClose() {
		if (dismissKey && !isWhatsNewDismissed(dismissKey)) {
			dismissWhatsNew(dismissKey);
		}
	}

	function handleViewChangelog() {
		handleDismiss();
		goto(withBasePath('/settings?tab=about'));
	}
</script>

<dialog
	bind:this={dialogEl}
	class="modal whats-new-modal"
	onclose={onDialogClose}
	aria-labelledby="whats-new-title"
>
	<div
		class="modal-box whats-new-box max-w-2xl animate-fade-in-up border border-accent/10 p-5 sm:p-8"
	>
		<IconButton
			size="sm"
			shape="square"
			icon={X}
			label="Close"
			class="absolute right-2 top-2"
			onclick={handleDismiss}
		/>

		<div class="mb-6 flex items-center gap-4">
			<div
				class="whats-new-icon-wrap relative flex h-11 w-11 items-center justify-center rounded-xl bg-accent/10 animate-glow-pulse"
			>
				<Sparkles class="text-accent h-5 w-5" />
			</div>
			<div>
				<h3 id="whats-new-title" class="text-xl font-bold tracking-tight">What's New</h3>
				{#if currentVersion}
					<p class="text-primary/70 mt-0.5 text-xs font-medium tracking-wide uppercase">
						{currentVersion}
					</p>
				{/if}
			</div>
		</div>

		<div class="divider my-0 opacity-10"></div>

		{#if sections.length > 0}
			<div
				class="whats-new-content release-notes-prose max-h-[55vh] max-w-none overflow-y-auto rounded-lg border border-base-content/5 bg-base-100/50 p-4 mt-4"
			>
				{#each sections as section, i (section.tag)}
					{#if i > 0}
						<div class="divider my-4 opacity-20"></div>
					{/if}
					<p
						class="text-base-content text-sm font-semibold border-l-2 border-accent/50 pl-3 {i > 0
							? ''
							: 'mt-0'} mb-3"
					>
						{section.name ?? section.tag}
					</p>
					<SafeHtml
						markdown={section.body}
						class="prose prose-sm max-w-none text-base-content/75"
					/>
				{/each}
			</div>
		{/if}

		<div class="modal-action mt-6 gap-3">
			<Button
				variant="ghost"
				size="sm"
				icon={ExternalLink}
				iconPosition="end"
				onclick={handleViewChangelog}
			>
				View full changelog
			</Button>
			<Button variant="accent" size="sm" onclick={handleDismiss}>Got it</Button>
		</div>
	</div>
	<form method="dialog" class="modal-backdrop">
		<button>close</button>
	</form>
</dialog>

<style>
	.whats-new-box {
		background:
			radial-gradient(
				ellipse at top left,
				oklch(from var(--color-accent) l c h / 0.04),
				transparent 50%
			),
			radial-gradient(
				ellipse at bottom right,
				oklch(from var(--color-primary) l c h / 0.03),
				transparent 50%
			),
			var(--color-base-200);
		box-shadow:
			0 0 0 1px oklch(from var(--color-accent) l c h / 0.06),
			0 24px 80px -12px rgba(0, 0, 0, 0.6);
	}

	.whats-new-content {
		scrollbar-width: thin;
		scrollbar-color: oklch(from var(--color-accent) l c h / 0.15) transparent;
	}

	dialog.whats-new-modal::backdrop {
		backdrop-filter: blur(16px) saturate(0.8);
		-webkit-backdrop-filter: blur(16px) saturate(0.8);
		background: rgba(0, 0, 0, 0.6);
	}

	@media (prefers-reduced-motion: reduce) {
		.whats-new-modal .modal-box {
			opacity: 1 !important;
		}
	}
</style>
