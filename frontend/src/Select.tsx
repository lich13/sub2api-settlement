import type { SelectHTMLAttributes } from "react";
import { ChevronDown } from "lucide-react";

export function Select({ className, children, ...props }: SelectHTMLAttributes<HTMLSelectElement>) {
  return <span className={`select-control${className === "filter" ? " select-filter" : ""}`}>
    <select {...props} className={className}>{children}</select>
    <ChevronDown className="select-arrow" size={15} aria-hidden="true" />
  </span>;
}
