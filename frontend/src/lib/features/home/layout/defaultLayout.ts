import {
	REFERENCE_COLUMNS,
	WIDGET_CONTRACT,
	type WidgetPlacement,
	type WidgetSpec
} from './contract';

/** Full-width widgets stacked in page order, each as tall as its content needs. */
export function defaultLayout(
	specs: WidgetSpec[],
	columns: number = REFERENCE_COLUMNS
): WidgetPlacement[] {
	let y = 0;
	return specs.map((spec) => {
		const { size } = WIDGET_CONTRACT[spec.kind];
		const h = spec.height ?? WIDGET_CONTRACT[spec.kind].defaultH;
		const placement: WidgetPlacement = {
			id: spec.id,
			kind: spec.kind,
			x: 0,
			y,
			w: columns,
			h,
			minW: size.minW,
			minH: spec.minH ?? size.minH,
			maxW: size.maxW,
			maxH: size.maxH
		};
		y += h;
		return placement;
	});
}
