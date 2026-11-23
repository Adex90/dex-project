/**
 * Legal beneficiaries (roadmap item 25, migration 0020), as the screens use
 * them. `lib/beneficiaries.ts` builds these from `GET /api/v1/kyc/beneficiarios`
 * after validating it, and turns drafts into the request.
 */

/** The server's relationship codes (kyc.beneficiarios.parentesco). */
export type Relationship = "CONYUGE" | "CONCUBINO" | "HIJO" | "PADRE" | "HERMANO" | "NIETO" | "ABUELO" | "OTRO";

/** A saved beneficiary. */
export interface Beneficiary {
  position: number;
  /** Upper-case legal name, as saved. */
  fullName: string;
  relationship: Relationship;
  curp: string | null;
  /** "YYYY-MM-DD". */
  birthDate: string | null;
  /** In whole cents: "33.34". */
  percent: string;
}

/** One saved version of the designation (immutable on the server). */
export interface Designation {
  version: number;
  savedAt: string;
  device: string | null;
  ip: string | null;
  /** SHA-256 of this version (chained to the previous one). */
  hash: string;
  previousHash: string;
  beneficiaries: Beneficiary[];
}

export interface DesignationSummary {
  version: number;
  savedAt: string;
  count: number;
  device: string | null;
  hash: string;
}

export interface BeneficiariesView {
  /** Null until the first designation. */
  current: Designation | null;
  /** The current version (0 if none): edits are saved on top of it. */
  version: number;
  /** Newest first. */
  history: DesignationSummary[];
  /** The server recomputed every version's hash and link. */
  chainVerified: boolean;
  /** Identity verified: the client may designate. */
  canDesignate: boolean;
  max: number;
}

/** A beneficiary being edited. */
export interface BeneficiaryDraft {
  /** Stable React key. */
  key: string;
  fullName: string;
  relationship: Relationship | "";
  curp: string;
  /** "YYYY-MM-DD" or "". */
  birthDate: string;
  /** As typed: "33.34". */
  percent: string;
}

/** The body of `POST /api/v1/kyc/beneficiarios`: the whole designation, saved on top of `version_base`. */
export interface DesignationRequest {
  beneficiarios: {
    nombre_completo: string;
    parentesco: Relationship | "";
    curp?: string;
    fecha_nacimiento?: string;
    porcentaje: string;
  }[];
  version_base: number;
}
