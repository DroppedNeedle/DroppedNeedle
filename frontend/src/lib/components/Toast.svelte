<script lang="ts">
	import { Check, X, Info, TriangleAlert } from 'lucide-svelte';
	import { TOAST_DURATION } from '$lib/constants';

	interface Props {
		show?: boolean;
		message?: string;
		type?: 'success' | 'error' | 'info' | 'warning';
		duration?: number;
	}

	let {
		show = $bindable(false),
		message = 'Added to Library',
		type = 'success',
		duration = TOAST_DURATION
	}: Props = $props();

	$effect(() => {
		if (show && duration > 0) {
			const timeout = setTimeout(() => {
				show = false;
			}, duration);
			return () => clearTimeout(timeout);
		}
	});

	const iconTintClasses: Record<string, string> = {
		success: 'text-success',
		error: 'text-error',
		info: 'text-info',
		warning: 'text-warning'
	};
</script>

{#if show}
	<div class="toast toast-end toast-bottom z-50">
		<div class="dn-toast flex items-center gap-3 rounded-2xl px-4 py-3">
			{#if type === 'success'}
				<Check class="h-5 w-5 shrink-0 {iconTintClasses[type]}" strokeWidth={2} />
			{:else if type === 'error'}
				<X class="h-5 w-5 shrink-0 {iconTintClasses[type]}" strokeWidth={2} />
			{:else if type === 'info'}
				<Info class="h-5 w-5 shrink-0 {iconTintClasses[type]}" strokeWidth={2} />
			{:else if type === 'warning'}
				<TriangleAlert class="h-5 w-5 shrink-0 {iconTintClasses[type]}" strokeWidth={2} />
			{/if}
			<span class="text-sm font-medium text-base-content/90">{message}</span>
		</div>
	</div>
{/if}

<style>
	.dn-toast {
		border: 1px solid var(--dn-hairline);
		background: oklch(from var(--color-base-200) l c h / 0.97);
		backdrop-filter: blur(24px) saturate(1.05);
		-webkit-backdrop-filter: blur(24px) saturate(1.05);
		box-shadow: var(--dn-shadow-4);
	}
</style>
