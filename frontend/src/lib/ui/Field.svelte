<!--
	The frame around one form control: a label tied to the control, help text
	and an error line. The control goes in the snippet and takes the ids it is
	handed. `aside` sits at the end of the label's row (a link to the
	field's reference).
	Replaces the `form-control` / `label-text` blocks and the bare
	`fieldset` / `fieldset-legend` blocks whose legend labels nothing.
-->
<script lang="ts">
	import type { Snippet } from 'svelte';

	interface ControlAttributes {
		id: string;
		'aria-describedby': string | undefined;
		'aria-invalid': boolean;
	}

	interface Props {
		label: string;
		help?: string;
		error?: string | null;
		/** Spread the argument onto the input, select or textarea. */
		children: Snippet<[ControlAttributes]>;
		aside?: Snippet;
	}

	let { label, help, error, children, aside }: Props = $props();

	const id = $props.id();
	const helpId = `${id}-help`;
	const errorId = `${id}-error`;

	const describedBy = $derived(
		[error ? errorId : null, help ? helpId : null].filter(Boolean).join(' ') || undefined
	);
</script>

<div class="fieldset">
	{#if aside}
		<div class="flex flex-wrap items-baseline justify-between gap-x-3">
			<label class="fieldset-legend" for={id}>{label}</label>
			{@render aside()}
		</div>
	{:else}
		<label class="fieldset-legend" for={id}>{label}</label>
	{/if}
	{@render children({ id, 'aria-describedby': describedBy, 'aria-invalid': Boolean(error) })}
	{#if help}
		<p id={helpId} class="label whitespace-normal text-muted">{help}</p>
	{/if}
	{#if error}
		<p id={errorId} class="text-sm text-error" role="alert">{error}</p>
	{/if}
</div>
