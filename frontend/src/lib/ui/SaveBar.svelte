<!--
	The row under a settings form: an optional Test button, a status line,
	Discard and Save. Save is live only while there are unsaved changes.
	Replaces the per-tab Save and Test buttons, their spinners and the mix of
	inline alerts and toasts that report the result.
-->
<script lang="ts">
	import { CircleCheck, CircleX, Info } from 'lucide-svelte';
	import AsyncButton from './AsyncButton.svelte';
	import Button from './Button.svelte';

	interface Status {
		tone: 'success' | 'error' | 'info';
		message: string;
	}

	interface Props {
		onsave: () => unknown;
		/** Leave unset on a form that does not track changes: Save stays live. */
		dirty?: boolean;
		saving?: boolean;
		/** Blocks Save and Test, e.g. while a required field is empty. */
		disabled?: boolean;
		saveLabel?: string;
		ondiscard?: () => void;
		ontest?: () => unknown;
		testing?: boolean;
		testLabel?: string;
		/** The last save or test result. */
		status?: Status | null;
	}

	let {
		onsave,
		dirty = true,
		saving = false,
		disabled = false,
		saveLabel = 'Save',
		ondiscard,
		ontest,
		testing = false,
		testLabel = 'Test',
		status = null
	}: Props = $props();

	const tones = { success: 'text-success', error: 'text-error', info: 'text-base-content/70' };
</script>

<div class="flex flex-wrap items-center gap-3">
	{#if ontest}
		<AsyncButton size="sm" onclick={ontest} pending={testing} {disabled}>
			{testLabel}
		</AsyncButton>
	{/if}
	<p
		class="flex min-w-0 flex-1 items-center gap-1.5 text-sm {status ? tones[status.tone] : ''}"
		role="status"
	>
		{#if status}
			{#if status.tone === 'success'}
				<CircleCheck class="size-4 shrink-0" aria-hidden="true" />
			{:else if status.tone === 'error'}
				<CircleX class="size-4 shrink-0" aria-hidden="true" />
			{:else}
				<Info class="size-4 shrink-0" aria-hidden="true" />
			{/if}
			<span>{status.message}</span>
		{/if}
	</p>
	{#if ondiscard && dirty}
		<Button variant="ghost" size="sm" onclick={ondiscard} disabled={saving}>Discard</Button>
	{/if}
	<AsyncButton
		variant="primary"
		size="sm"
		onclick={onsave}
		pending={saving}
		disabled={disabled || !dirty}
	>
		{saveLabel}
	</AsyncButton>
</div>
