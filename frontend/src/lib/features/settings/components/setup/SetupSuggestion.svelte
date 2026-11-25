<!--
	One thing the setup guide found, with its settings form below once the
	admin asks to set it up. Nothing here saves: the form's own Save does, and
	Skip closes the form, dropping what was typed.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import StatusChip from '$lib/ui/StatusChip.svelte';

	interface Props {
		title: string;
		/** The address or folder found, shown as found. */
		detail: string;
		/** A quieter line under it: a count or a version. */
		meta?: string;
		/** Something to do first, shown as a warning. */
		note?: string;
		/** It is already set up. */
		done?: boolean;
		open: boolean;
		setupLabel?: string;
		onopen: () => void;
		ondismiss?: () => void;
		onskip: () => void;
		children: Snippet;
	}

	let {
		title,
		detail,
		meta,
		note,
		done = false,
		open,
		setupLabel = 'Set up',
		onopen,
		ondismiss,
		onskip,
		children
	}: Props = $props();
</script>

<Card variant="bordered" padding="md">
	<div class="flex flex-wrap items-start justify-between gap-3">
		<div class="min-w-0 flex-1 space-y-1">
			<div class="flex flex-wrap items-center gap-2">
				<h3 class="font-semibold">{title}</h3>
				{#if done}<StatusChip tone="success" size="xs">Set up</StatusChip>{/if}
			</div>
			<p class="font-mono text-sm break-all">{detail}</p>
			{#if meta}<p class="text-xs text-muted">{meta}</p>{/if}
			{#if note}<p class="text-xs text-warning">{note}</p>{/if}
		</div>
		<div class="flex flex-wrap gap-2">
			{#if open}
				<Button variant="ghost" size="sm" onclick={onskip}>Skip</Button>
			{:else}
				<Button variant="primary" size="sm" onclick={onopen}>{setupLabel}</Button>
				{#if ondismiss}
					<Button variant="ghost" size="sm" onclick={ondismiss}>Not this</Button>
				{/if}
			{/if}
		</div>
	</div>
	{#if open}
		<div class="mt-4">{@render children()}</div>
	{/if}
</Card>
