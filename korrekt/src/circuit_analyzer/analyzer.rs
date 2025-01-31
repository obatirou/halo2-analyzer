use anyhow::{Context, Result, Ok};
use halo2_proofs::{
    arithmetic::FieldExt as Field,
    circuit::layouter::RegionColumn,
    dev::CellValue,
    plonk::{Circuit, ConstraintSystem, Expression},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    fs::File,
    fs::OpenOptions,
    path::Path,
    process::Command,
};

use crate::circuit_analyzer::{
    abstract_expr::{self, AbsResult},
    layouter,
};
use crate::io::analyzer_io::{output_result, retrieve_user_input_for_underconstrained};
use crate::io::analyzer_io_type::{
    AnalyzerInput, AnalyzerOutput, AnalyzerOutputStatus, AnalyzerType, VerificationMethod,
};
use crate::smt_solver::{
    smt,
    smt::Printer,
    smt_parser::{self, ModelResult, Satisfiability},
};
use layouter::AnalyticLayouter;

#[derive(Debug)]
pub struct Analyzer<F: Field> {
    pub cs: ConstraintSystem<F>,
    pub layouter: layouter::AnalyticLayouter<F>,
    pub log: Vec<String>,
    pub counter: u32,
}
#[derive(Debug)]
pub enum NodeType {
    Constant,
    Advice,
    Instance,
    Fixed,
    Negated,
    Mult,
    Add,
    Scaled,
    Poly,
}
#[derive(Debug)]
pub enum Operation {
    Equal,
    NotEqual,
    And,
    Or,
}

/// Creates an `Analyzer` instance with a circuit.
///
/// This function creates an `Analyzer` instance by synthesizing the provided `Circuit` with an analytic layouter.
/// It internally creates a constraint system to collect custom gates and uses the `circuit` parameter to synthesize the circuit
/// and populate the analytic layouter. The function returns the resulting `Analyzer` instance.
///
impl<F: Field, C: Circuit<F>> From<&C> for Analyzer<F> {
    fn from(circuit: &C) -> Self {
        // create constraint system to collect custom gates
        let mut cs: ConstraintSystem<F> = Default::default();
        let config = C::configure(&mut cs);
        // synthesize the circuit with analytic layout
        let mut layouter = AnalyticLayouter::new();
        circuit.synthesize(config, &mut layouter).unwrap();
        Analyzer {
            cs,
            layouter,
            log: vec![],
            counter: 0,
        }
    }
}
impl<'b, F: Field> Analyzer<F> {
    /// Detects unused custom gates
    ///
    /// This function iterates through the gates in the constraint system (`self.cs`) and checks if each gate is used.
    /// A gate is considered unused if it evaluates to zero for all regions in the layouter (`self.layouter`).
    /// If an unused gate is found, it is logged in the `self.log` vector along with a suggested action.
    /// Finally, the function prints the total number of unused gates found.
    ///
    pub fn analyze_unused_custom_gates(&mut self) -> Result<AnalyzerOutput> {
        let mut count = 0;
        let mut used;
        for gate in self.cs.gates.iter() {
            used = false;

            // is this gate identically zero over regions?
            'region_search: for region in self.layouter.regions.iter() {
                let selectors = HashSet::from_iter(region.selectors().into_iter());
                for poly in gate.polynomials() {
                    let res = abstract_expr::eval_abstract(poly, &selectors);
                    if res != AbsResult::Zero {
                        used = true;
                        break 'region_search;
                    }
                }
            }

            if !used {
                count += 1;
                self.log.push(format!("unused gate: \"{}\" (consider removing the gate or checking selectors in regions)", gate.name()));
            }
        }
        println!("Finished analysis: {} unused gates found.", count);
        Ok(AnalyzerOutput {
            output_status: AnalyzerOutputStatus::UnusedCustomGates,
        })
    }

    /// Detects unused columns
    ///
    /// This function iterates through the advice queries in the constraint system (`self.cs`) and checks if each column is used.
    /// A column is considered unused if it does not appear in any of the polynomials within the gates of the constraint system.
    /// If an unused column is found, it is logged in the `self.log` vector.
    /// Finally, the function prints the total number of unused columns found.
    ///
    pub fn analyze_unused_columns(&mut self) -> Result<AnalyzerOutput> {
        let mut count = 0;
        let mut used;
        for (column, rotation) in self.cs.advice_queries.iter().cloned() {
            used = false;

            for gate in self.cs.gates.iter() {
                for poly in gate.polynomials() {
                    let advices = abstract_expr::extract_columns(poly);
                    if advices.contains(&(column.into(), rotation)) {
                        used = true;
                    }
                }
            }

            if !used {
                count += 1;
                self.log.push(format!("unused column: {:?}", column));
            }
        }
        println!("Finished analysis: {} unused columns found.", count);
        Ok(AnalyzerOutput {
            output_status: AnalyzerOutputStatus::UnusedColumns,
        })
    }

    /// Detect assigned but unconstrained cells:
    /// (does it occur in a not-identially zero polynomial in the region?)
    /// (if not almost certainly a bug)
    pub fn analyze_unconstrained_cells(&mut self) -> Result<AnalyzerOutput> {
        let mut count = 0;
        for region in self.layouter.regions.iter() {
            let selectors = HashSet::from_iter(region.selectors().into_iter());
            let mut used;
            for (reg_column, rotation) in region.columns.iter().cloned() {
                used = false;

                match reg_column {
                    RegionColumn::Selector(_) => continue,
                    RegionColumn::Column(column) => {
                        for gate in self.cs.gates.iter() {
                            for poly in gate.polynomials() {
                                let advices = abstract_expr::extract_columns(poly);
                                let eval = abstract_expr::eval_abstract(poly, &selectors);

                                if eval != AbsResult::Zero && advices.contains(&(column, rotation))
                                {
                                    used = true;
                                }
                            }
                        }
                    }
                };

                if !used {
                    count += 1;
                    self.log.push(format!("unconstrained cell in \"{}\" region: {:?} (rotation: {:?}) -- very likely a bug.", region.name,  reg_column, rotation));
                }
            }
        }
        println!("Finished analysis: {} unconstrained cells found.", count);
        Ok(AnalyzerOutput {
            output_status: AnalyzerOutputStatus::UnconstrainedCells,
        })
    }
    /// Extracts instance columns from an equality table.
    ///
    /// This function takes an equality table (`eq_table`) represented as a `HashMap` with cell names as keys
    /// and corresponding strings as values. It creates a new `HashMap` (`instance_cols_string`) and populates it
    /// with the keys from the `eq_table`, assigning an initial value of zero to each key. The resulting `HashMap`
    /// represents the extracted instance columns.
    pub fn extract_instance_cols(
        &mut self,
        eq_table: HashMap<String, String>,
    ) -> HashMap<String, i64> {
        let mut instance_cols_string: HashMap<String, i64> = HashMap::new();
        for cell in eq_table {
            instance_cols_string.insert(cell.0.to_owned(), 0);
        }
        instance_cols_string
    }
    /// Extracts instance columns from regions in the layouter.
    ///
    /// This function iterates through the regions in the layouter (`self.layouter`) and extracts the instance columns
    /// from each region's equality table. It creates a new `HashMap` (`instance_cols_string`) and populates it with
    /// the extracted instance columns, assigning an initial value of zero to each column. The resulting `HashMap`
    /// represents the extracted instance columns from all regions in the layouter.
    ///
    pub fn extract_instance_cols_from_region(&mut self) -> HashMap<String, i64> {
        let mut instance_cols_string: HashMap<String, i64> = HashMap::new();
        for region in self.layouter.regions.iter() {
            for eq_adv in region.eq_table.iter() {
                instance_cols_string.insert(eq_adv.0.to_owned(), 0);
            }
        }
        instance_cols_string
    }

    /// Analyzes underconstrained circuits and generates an analyzer output.
    ///
    /// This function performs the analysis of underconstrained circuits. It takes as input an `analyzer_input` struct
    /// containing the required information for the analysis, and a `fixed` vector representing the vallues of fixed columns.
    /// The function creates an SMT file, decomposes the polynomials, writes the necessary variables and assertions, and runs
    /// the control uniqueness function to determine the `output_status` of the analysis.
    /// The analyzer output is returned as a `Result` indicating success or an error if the analysis fails.
    pub fn analyze_underconstrained(
        &mut self,
        analyzer_input: AnalyzerInput,
        fixed: Vec<Vec<CellValue<F>>>,
        base_field_prime: &str,
    ) -> Result<AnalyzerOutput> {
        fs::create_dir_all("src/output/").unwrap();
        let smt_file_path = "src/output/out.smt2";
        let mut smt_file =
            std::fs::File::create(smt_file_path).context("Failed to create file!")?;
        let mut printer = smt::write_start(&mut smt_file, base_field_prime.to_owned());

        Self::decompose_polynomial(self, &mut printer, fixed);

        let instance_string = analyzer_input.verification_input.instances_string.clone();

        let mut analyzer_output: AnalyzerOutput = AnalyzerOutput {
            output_status: AnalyzerOutputStatus::Invalid,
        };
        for region in self.layouter.regions.iter() {
            for eq_adv in region.advice_eq_table.iter() {
                smt::write_var(&mut printer, eq_adv.0.to_owned());
                smt::write_var(&mut printer, eq_adv.1.to_owned());

                let neg = format!("(ff.neg {})", eq_adv.1);
                let term = smt::write_term(
                    &mut printer,
                    "add".to_owned(),
                    eq_adv.0.to_owned(),
                    NodeType::Advice,
                    neg,
                    NodeType::Advice,
                );
                smt::write_assert(
                    &mut printer,
                    term,
                    "0".to_owned(),
                    NodeType::Poly,
                    Operation::Equal,
                );
            }
        }

        for region in self.layouter.regions.iter() {
            for eq_adv in region.eq_table.iter() {
                smt::write_var(&mut printer, eq_adv.0.to_owned());
                smt::write_var(&mut printer, eq_adv.1.to_owned());

                let neg = format!("(ff.neg {})", eq_adv.1);
                let term = smt::write_term(
                    &mut printer,
                    "add".to_owned(),
                    eq_adv.0.to_owned(),
                    NodeType::Advice,
                    neg,
                    NodeType::Advice,
                );
                smt::write_assert(
                    &mut printer,
                    term,
                    "0".to_owned(),
                    NodeType::Poly,
                    Operation::Equal,
                );
            }
        }

        let output_status: AnalyzerOutputStatus = Self::uniqueness_assertion(
            smt_file_path.to_owned(),
            &instance_string,
            &analyzer_input,
            &mut printer,
        )
        .context("Failed to run control uniqueness function!")?;

        analyzer_output.output_status = output_status;
        output_result(analyzer_input, &analyzer_output);

        Ok(analyzer_output)
    }

    #[cfg(test)]
    pub fn log(&self) -> &[String] {
        &self.log
    }
    /**
     * Decomposes an `Expression` into its corresponding SMT-LIB format (`String`) and its type (`NodeType`).
     *
     * # Arguments
     *
     * * `poly` - A reference to an `Expression` instance that is to be decomposed into SMT-LIB v2 format.
     * * `printer` - A mutable reference to a `Printer` instance which is used for writing the decomposed expression.
     * * `region_no` - An integer that represents the region number.
     * * `row_num` - An integer that represents the row number in region.
     * * `es` - A reference to a `HashSet` of Strings representing enabled selectors. These selectors are checked during the decomposition.
     *
     * # Returns
     *
     * A tuple of two elements:
     * * `String` - The SMT-LIB v2 formatted string representation of the decomposed expression.
     * * `NodeType` - The type of the node that the expression corresponds to.
     *
     * # Behavior
     *
     * The function formats a string representation of the expression in SMT-LIB v2 format and identifies its polynomial type as a NodeType.
     *  The function has a recursive behavior in the cases of `Negated`, `Sum`, `Product`,
     * and `Scaled` variants of `Expression`, where it decomposes the nested expressions by calling itself.
     */
    fn decompose_expression(
        poly: &Expression<F>,
        printer: &mut smt::Printer<File>,
        region_no: usize,
        row_num: i32,
        es: &HashSet<String>,
    ) -> (String, NodeType) {
        match &poly {
            Expression::Constant(a) => {
                let term = format!("(as ff{} F)", a.get_lower_128());
                (term, NodeType::Constant)
            }
            Expression::Selector(a) => {
                let s = format!("S-{:?}-{}-{}", region_no, a.0, row_num);
                if es.contains(&s) {
                    ("(as ff1 F)".to_owned(), NodeType::Fixed)
                } else {
                    ("(as ff0 F)".to_owned(), NodeType::Fixed)
                }
            }
            Expression::Fixed(fixed_query) => {
                let term = format!(
                    "F-{}-{}-{}",
                    region_no,
                    fixed_query.column_index,
                    fixed_query.rotation.0 + row_num
                );

                (term, NodeType::Fixed)
            }
            Expression::Advice(advice_query) => {
                let term = format!(
                    "A-{}-{}-{}",
                    region_no,
                    advice_query.column_index,
                    advice_query.rotation.0 + row_num
                );
                smt::write_var(printer, term.clone());
                (term, NodeType::Advice)
            }
            Expression::Instance(_instance_query) => ("".to_owned(), NodeType::Instance),
            Expression::Negated(poly) => {
                let (node_str, node_type) =
                    Self::decompose_expression(poly, printer, region_no, row_num, es);
                let term = if (matches!(node_type, NodeType::Advice)
                    || matches!(node_type, NodeType::Instance)
                    || matches!(node_type, NodeType::Fixed)
                    || matches!(node_type, NodeType::Constant))
                {
                    format!("ff.neg {}", node_str)
                } else {
                    format!("ff.neg ({})", node_str)
                };
                //let term = format!("ff.neg {}", node_str);
                (term, NodeType::Negated)
            }
            Expression::Sum(a, b) => {
                let (node_str_left, nodet_type_left) =
                    Self::decompose_expression(a, printer, region_no, row_num, es);
                let (node_str_right, nodet_type_right) =
                    Self::decompose_expression(b, printer, region_no, row_num, es);
                let term = smt::write_term(
                    printer,
                    "add".to_owned(),
                    node_str_left,
                    nodet_type_left,
                    node_str_right,
                    nodet_type_right,
                );
                (term, NodeType::Add)
            }
            Expression::Product(a, b) => {
                let (node_str_left, nodet_type_left) =
                    Self::decompose_expression(a, printer, region_no, row_num, es);
                let (node_str_right, nodet_type_right) =
                    Self::decompose_expression(b, printer, region_no, row_num, es);
                let term = smt::write_term(
                    printer,
                    "mul".to_owned(),
                    node_str_left,
                    nodet_type_left,
                    node_str_right,
                    nodet_type_right,
                );
                (term, NodeType::Mult)
            }
            Expression::Scaled(_poly, c) => {
                // convering the field element into an expression constant and recurse.
                let (node_str_left, nodet_type_left) = Self::decompose_expression(
                    &Expression::Constant(*c),
                    printer,
                    region_no,
                    row_num,
                    es,
                );
                let (node_str_right, nodet_type_right) =
                    Self::decompose_expression(_poly, printer, region_no, row_num, es);
                let term = smt::write_term(
                    printer,
                    "mul".to_owned(),
                    node_str_left,
                    nodet_type_left,
                    node_str_right,
                    nodet_type_right,
                );
                (term, NodeType::Scaled)
            }
        }
    }
    /// Decomposes polynomials and writes assertions using an SMT printer.
    ///
    /// This function iterates over the regions and rows of a layouter and decomposes the polynomials
    /// associated with gates and lookups. It writes assertions using an SMT printer based on the decomposed expressions.
    ///
    pub fn decompose_polynomial(
        &'b mut self,
        printer: &mut smt::Printer<File>,
        fixed: Vec<Vec<CellValue<F>>>,
    ) ->Result<(), anyhow::Error>{
        if !self.layouter.regions.is_empty() {
            for region_no in 0..self.layouter.regions.len() {
                for row_num in 0..self.layouter.regions[region_no].row_count {
                    for gate in self.cs.gates.iter() {
                        for poly in &gate.polys {
                            let (node_str, _) = Self::decompose_expression(
                                poly,
                                printer,
                                region_no,
                                i32::try_from(row_num).ok().unwrap(),
                                &self.layouter.regions[region_no].enabled_selectors,
                            );

                            smt::write_assert(
                                printer,
                                node_str,
                                "0".to_owned(),
                                NodeType::Poly,
                                Operation::Equal,
                            );
                        }
                    }
                }
            }

            for region_no in 0..self.layouter.regions.len() {
                for row_num in 0..self.layouter.regions[region_no].row_count {
                    for lookup in self.cs.lookups.iter() {
                        let mut cons_str_vec = Vec::new();
                        for poly in &lookup.input_expressions {
                            let (node_str, _) = Self::decompose_expression(
                                poly,
                                printer,
                                region_no,
                                i32::try_from(row_num).ok().unwrap(),
                                &self.layouter.regions[region_no].enabled_selectors,
                            );
                            cons_str_vec.push(node_str);
                        }
                        let mut exit = false;
                        let mut col_indices = Vec::new();
                        for col in lookup.table_expressions.clone() {
                            if exit {
                                break;
                            }
                            if let Expression::Fixed(fixed_query) = col {
                                col_indices.push(fixed_query.column_index);
                            }
                        }
                        let mut big_cons_str = "".to_owned();
                        let mut big_cons = vec![];
                        for row in 0..fixed[0].len() {
                            //*** Iterate over look up table rows */
                            if exit {
                                break;
                            }
                            let mut equalities = vec![];
                            let mut eq_str = String::new();
                            for col in 0..col_indices.len() {
                                //*** Iterate over fixed cols */
                                let mut t = String::new();
                                match fixed[col_indices[col]][row] {
                                    CellValue::Unassigned => {
                                        exit = true;
                                        break;
                                    }
                                    CellValue::Assigned(f) => {
                                        t = f.get_lower_128().to_string();
                                    }
                                    CellValue::Poison(_) => {}
                                }
                                if let CellValue::Assigned(value) = fixed[col_indices[col]][row] {
                                    t = value.get_lower_128().to_string();
                                }
                                let sa = smt::get_assert(
                                    printer,
                                    cons_str_vec[col].clone(),
                                    t,
                                    NodeType::Mult,
                                    Operation::Equal,
                                ).context("Failled to generate assert!")?;
                                equalities.push(sa);
                            }
                            if exit {
                                break;
                            }
                            for var in equalities.iter() {
                                eq_str.push_str(var);
                            }
                            let and_eqs = smt::get_and(printer, eq_str);

                            big_cons.push(and_eqs);
                        }
                        for var in big_cons.iter() {
                            big_cons_str.push_str(var);
                        }

                        smt::write_assert_bool(printer, big_cons_str, Operation::Or);
                    }
                }
            }
        }
        Ok(())
    }
    /// Checks the uniqueness inputs and returns the analysis result.
    ///
    /// This function checks the uniqueness by solving SMT formulas with various assignments
    /// and constraints. It iterates over the variables and applies different rules based on the verification method
    /// specified in the `analyzer_input`. The function writes assertions using an SMT printer and returns the
    /// analysis result as `AnalyzerOutputStatus`.
    ///
    pub fn uniqueness_assertion(
        smt_file_path: String,
        instance_cols_string: &HashMap<String, i64>,
        analyzer_input: &AnalyzerInput,
        printer: &mut smt::Printer<File>,
    ) -> Result<AnalyzerOutputStatus> {
        let mut result: AnalyzerOutputStatus = AnalyzerOutputStatus::NotUnderconstrainedLocal;
        let mut variables: HashSet<String> = HashSet::new();
        for variable in printer.vars.keys() {
            variables.insert(variable.clone());
        }

        let mut max_iterations: u128 = 1;

        match analyzer_input.verification_method {
            VerificationMethod::Specific => {
                for var in instance_cols_string {
                    smt::write_var(printer, var.0.to_owned());
                    smt::write_assert(
                        printer,
                        var.0.clone(),
                        (*var.1).to_string(),
                        NodeType::Instance,
                        Operation::Equal,
                    );
                }
            }
            VerificationMethod::Random => {
                max_iterations = analyzer_input.verification_input.iterations;
            }
        }
        let model = Self::solve_and_get_model(smt_file_path.clone(), &variables)
            .context("Failed to solve and get model!")?;
        if matches!(model.sat, Satisfiability::Unsatisfiable) {
            result = AnalyzerOutputStatus::Overconstrained;
            return Ok(result); // We can just break here.
        }
        for i in 1..=max_iterations {
            let model = Self::solve_and_get_model(smt_file_path.clone(), &variables)
                .context("Failed to solve and get model!")?;
            if matches!(model.sat, Satisfiability::Unsatisfiable) {
                result = AnalyzerOutputStatus::NotUnderconstrained;
                return Ok(result); // We can just break here.
            }

            println!("Model {} to be checked:", i);
            for r in &model.result {
                println!("{} : {}", r.1.name, r.1.value.element)
            }

            // Imitate the creation of a new solver by utilizing the stack functionality of solver
            smt::write_push(printer, 1);

            //*** To check the model is under-constrained we need to:
            //      1. Fix the public input
            //      2. Change the other vars
            //      3. add these rules to the current solver and,
            //      4. find a model that satisfies these rules

            let mut same_assignments = vec![];
            let mut diff_assignments = vec![];
            for var in variables.iter() {
                // The second condition is needed because the following constraints would've been added already to the solver in the beginning.
                // It is not strictly necessary, but there is no point in adding redundant constraints to the solver.
                if instance_cols_string.contains_key(var)
                    && !matches!(
                        analyzer_input.verification_method,
                        VerificationMethod::Specific
                    )
                {
                    // 1. Fix the public input
                    let result_from_model = &model.result[var];
                    let sa = smt::get_assert(
                        printer,
                        result_from_model.name.clone(),
                        result_from_model.value.element.clone(),
                        NodeType::Instance,
                        Operation::Equal,
                    ).context("Failled to generate assert!")?;
                    same_assignments.push(sa);
                } else {
                    //2. Change the other vars
                    let result_from_model = &model.result[var];
                    let sa = smt::get_assert(
                        printer,
                        result_from_model.name.clone(),
                        result_from_model.value.element.clone(),
                        NodeType::Instance,
                        Operation::NotEqual,
                    ).context("Failled to generate assert!")?;
                    diff_assignments.push(sa);
                }
            }

            let mut same_str = "".to_owned();
            for var in same_assignments.iter() {
                same_str.push_str(var);
            }
            let mut diff_str = "".to_owned();
            for var in diff_assignments.iter() {
                diff_str.push_str(var);
            }

            // 3. add these rules to the current solver,
            let or_diff_assignments = smt::get_or(printer, diff_str);
            same_str.push_str(&or_diff_assignments);
            let and_all = smt::get_and(printer, same_str);
            smt::write_assert_bool(printer, and_all, Operation::And);

            // 4. find a model that satisfies these rules
            let model_with_constraint =
                Self::solve_and_get_model(smt_file_path.clone(), &variables)
                    .context("Failed to solve and get model!")?;
            if matches!(model_with_constraint.sat, Satisfiability::Satisfiable) {
                println!("Equivalent model for the same public input:");
                for r in &model_with_constraint.result {
                    println!("{} : {}", r.1.name, r.1.value.element)
                }
                result = AnalyzerOutputStatus::Underconstrained;
                return Ok(result);
            } else {
                println!("There is no equivalent model with the same public input to prove model {} is under-constrained!", i);
            }
            smt::write_pop(printer, 1);

            // If no model found, add some rules to the initial solver to make sure does not generate the same model again
            let mut negated_model_variable_assignments = vec![];
            for res in &model.result {
                if instance_cols_string.contains_key(&res.1.name) {
                    let sa = smt::get_assert(
                        printer,
                        res.1.name.clone(),
                        res.1.value.element.clone(),
                        NodeType::Instance,
                        Operation::NotEqual,
                    ).context("Failled to generate assert!")?;
                    negated_model_variable_assignments.push(sa);
                }
            }
            let mut neg_model = "".to_owned();
            for var in negated_model_variable_assignments.iter() {
                neg_model.push_str(var);
            }
            smt::write_assert_bool(printer, neg_model, Operation::Or);
        }
        Ok(result)
    }
    /// Generates a copy path for the SMT file.
    ///
    /// This function takes the original SMT file path as input and generates a copy path
    /// for the SMT file. The copy path is constructed by appending "_temp.smt2" to the
    /// original file's stem (i.e., file name without extension), and placing it in the
    /// "src/output/" directory. The function then creates a copy of the original file
    /// at the generated copy path.
    ///
    pub fn generate_copy_path(smt_file_path: String) -> Result<String> {
        let smt_path_clone = smt_file_path.clone();
        let smt_path_obj = Path::new(&smt_path_clone);
        let smt_file_stem = smt_path_obj.file_stem().unwrap();
        let smt_file_copy_path = format!(
            "{}{}{}",
            "src/output/",
            smt_file_stem.to_str().unwrap(),
            "_temp.smt2"
        );
        fs::copy(smt_file_path, smt_file_copy_path.clone()).context("Failed to copy file!")?;
        Ok(smt_file_copy_path)
    }
    // Solves the SMT formula in the specified file and retrieves the model result.
    ///
    /// This function solves the SMT formula in the given `smt_file_path` by executing the CVC5 solver.
    /// It appends the necessary commands to the SMT file for checking satisfiability and retrieving values
    /// for the specified variables. The function then runs the CVC5 solver and captures its output.
    /// The output is parsed to extract the model result, which is returned as a `ModelResult`.
    ///
    pub fn solve_and_get_model(
        smt_file_path: String,
        variables: &HashSet<String>,
    ) -> Result<ModelResult> {
        let smt_file_copy_path =
            Self::generate_copy_path(smt_file_path).context("Failed to generate copy path!")?;
        let mut smt_file_copy = OpenOptions::new()
            .append(true)
            .open(smt_file_copy_path.clone())
            .expect("cannot open file");
        let mut copy_printer = Printer::new(&mut smt_file_copy);

        // Add (check-sat) (get-value var) ... here.
        smt::write_end(&mut copy_printer);
        for var in variables.iter() {
            smt::write_get_value(&mut copy_printer, var.clone());
        }
        let output = Command::new("cvc5").arg(smt_file_copy_path).output();
        let term = output.unwrap();
        let output_string = String::from_utf8_lossy(&term.stdout);

        smt_parser::extract_model_response(output_string.to_string())
            .context("Failed to parse smt result!")
    }
    /// Dispatches the analysis based on the specified analyzer type.
    ///
    /// This function takes an `AnalyzerType` enum and performs the corresponding analysis
    /// based on the type. The supported analyzer types include:
    ///
    /// - `UnusedGates`: Analyzes and identifies unused custom gates in the circuit.
    /// - `UnconstrainedCells`: Analyzes and identifies cells with unconstrained values in the circuit.
    /// - `UnusedColumns`: Analyzes and identifies unused columns in the circuit.
    /// - `UnderconstrainedCircuit`: Analyzes the circuit for underconstrained properties by
    ///   retrieving user input for specific instance columns and conducting analysis.
    ///
    /// The function performs the analysis and updates the internal state accordingly.
    ///
    pub fn dispatch_analysis(
        &mut self,
        analyzer_type: AnalyzerType,
        fixed: Vec<Vec<CellValue<F>>>,
        prime: &str,
    ) -> Result<AnalyzerOutput> {
        match analyzer_type {
            AnalyzerType::UnusedGates => self.analyze_unused_custom_gates(),
            AnalyzerType::UnconstrainedCells => self.analyze_unconstrained_cells(),
            AnalyzerType::UnusedColumns => self.analyze_unused_columns(),
            AnalyzerType::UnderconstrainedCircuit => {
                let mut instance_cols_string =
                    self.extract_instance_cols(self.layouter.eq_table.clone());
                let instance_cols_string_from_region = self.extract_instance_cols_from_region();
                instance_cols_string.extend(instance_cols_string_from_region);
                let analyzer_input: AnalyzerInput =
                    retrieve_user_input_for_underconstrained(&instance_cols_string)
                        .context("Failed to retrieve user input!")?;
                self.analyze_underconstrained(analyzer_input, fixed, prime)
            }
        }
    }
}
