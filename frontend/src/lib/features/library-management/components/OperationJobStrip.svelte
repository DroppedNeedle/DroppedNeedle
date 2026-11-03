<!--
	The pause, resume and stop buttons of a background job. It shows only what
	the job's state allows: Pause while it runs, Resume while it is paused,
	Stop while it is queued, running or paused, and "Pausing..." or
	"Stopping..." while such a request waits. Each button stays busy until its
	promise settles.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';
	import { CirclePause, CirclePlay, OctagonX } from 'lucide-svelte';

	import AsyncButton from '$lib/ui/AsyncButton.svelte';
	import Button from '$lib/ui/Button.svelte';

	interface Props {
		status: string;
		/** The job's waiting control request (`none`, `pause` or `stop`), where it reports one. */
		controlRequest?: string;
		size?: 'xs' | 'sm';
		/** Names the job in each button's accessible name, e.g. "local scan". */
		subject?: string;
		onpause?: () => unknown;
		onresume?: () => unknown;
		/** Gets the Stop button, so a confirmation can hand focus back to it. */
		onstop?: (opener: HTMLElement) => unknown;
		/** Stop opens a confirmation first, so it reads "Stop...". */
		confirmStop?: boolean;
		class?: string;
		/** More actions after the controls, such as a details link. */
		children?: Snippet;
	}

	let {
		status,
		controlRequest = 'none',
		size = 'sm',
		subject,
		onpause,
		onresume,
		onstop,
		confirmStop = false,
		class: className = '',
		children
	}: Props = $props();

	let stopButton = $state<HTMLElement | null>(null);

	const active = $derived(['queued', 'running', 'paused'].includes(status));
	const waiting = $derived(
		controlRequest === 'stop' ? 'Stopping...' : controlRequest === 'pause' ? 'Pausing...' : null
	);
	const canResume = $derived(!waiting && status === 'paused' && onresume !== undefined);
	const canPause = $derived(!waiting && status === 'running' && onpause !== undefined);
	const canStop = $derived(active && controlRequest !== 'stop' && onstop !== undefined);
	const shown = $derived(Boolean(waiting) || canResume || canPause || canStop || Boolean(children));

	function named(action: string): string | undefined {
		return subject ? `${action} ${subject}` : undefined;
	}
</script>

{#if shown}
	<div class="flex flex-wrap items-center gap-2 {className}">
		{#if waiting}
			<Button variant="ghost" {size} loading>{waiting}</Button>
		{:else if canResume}
			<AsyncButton
				variant="outline"
				{size}
				icon={CirclePlay}
				aria-label={named('Resume')}
				onclick={() => onresume?.()}>Resume</AsyncButton
			>
		{:else if canPause}
			<AsyncButton
				variant="outline"
				{size}
				icon={CirclePause}
				aria-label={named('Pause')}
				onclick={() => onpause?.()}>Pause</AsyncButton
			>
		{/if}
		{#if canStop}
			<AsyncButton
				variant="danger-ghost"
				{size}
				icon={OctagonX}
				aria-label={named('Stop')}
				disabled={controlRequest === 'pause'}
				bind:ref={stopButton}
				onclick={() => stopButton && onstop?.(stopButton)}
				>{confirmStop ? 'Stop...' : 'Stop'}</AsyncButton
			>
		{/if}
		{@render children?.()}
	</div>
{/if}
